// "Agent Memory and Rules" over the kernel's routes (spec 3.3 §5.1, §8.1, §8.2, §10 K5,
// §12 phases 3 and 4): what agents remember and the person's corrections, rules allowed
// without asking with their end dates and Extend, the record and its check, and each area's
// agents with what they spent today.
import { render, screen, waitFor, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, describe, expect, it, vi } from "vitest";
import { App } from "../src/App";
import { HttpClient } from "../src/client";
import { areaAgents } from "../src/components/AreaRoster";
import { onThisDevice, zoneOffset } from "../src/away";
import { FixtureClient, agents, fixtureSeams, recordLines } from "../src/fixture/fixture";
import type { AgentRow } from "../src/generated/api";
import type { KernelRecordLine, KernelTidyUp, StandingRule } from "../src/kernel";
import { cronWeekdays, nextFire } from "../src/kernel";
import { factOf, memoryOf, tidyUpText, undoText } from "../src/memory";
import { recentLines, recordSentence } from "../src/record";
import { ended, ruleSentence } from "../src/rules";
import type { MemoryFact, MemorySeam, Seams } from "../src/seams";

afterEach(() => vi.unstubAllGlobals());

const rows: AgentRow[] = [
  { agent: agents.finance, status: "working", statusText: "Working", sessionId: "s-finance" },
  { agent: agents.code, status: "waitingForYou", statusText: "Waiting for you", sessionId: "s-code" },
];

/** A memory the corrections change, as the kernel's book does. */
function memory(): { seam: MemorySeam; calls: string[] } {
  let facts: MemoryFact[] = [
    { id: "f1", text: "You bill [CLIENT] at [RATE] an hour.", provenance: "youSaidThis" },
    { id: "f2", text: "Your accountant is [NAME].", provenance: "aGuess", noticedBy: "Finance" },
    { id: "f3", text: "You send invoices on the last working day.", provenance: "aGuess", noticedBy: "Finance" },
  ];
  let tidyUp: { id: string; text: string } | undefined = { id: "tick-1", text: "Overnight, your agents merged two notes into one." };
  const calls: string[] = [];
  const seam: MemorySeam = {
    facts: async () => ({ facts, tidyUp }),
    confirm: async (id) => {
      calls.push(`confirm ${id}`);
      facts = facts.map((fact) => (fact.id === id ? { ...fact, provenance: "youConfirmedThis", noticedBy: undefined } : fact));
    },
    fix: async (id, text) => {
      calls.push(`fix ${id} ${text}`);
      facts = facts.map((fact) => (fact.id === id ? { id: "f9", text, provenance: "youConfirmedThis" } : fact));
    },
    forget: async (id) => {
      calls.push(`forget ${id}`);
      facts = facts.filter((fact) => fact.id !== id);
    },
    undo: async (id) => {
      calls.push(`undo ${id}`);
      tidyUp = undefined;
      facts = [...facts, { id: "f4", text: "Your accountant's email is [EMAIL].", provenance: "aGuess" }];
      return { restored: 2, forgotten: 1, kept: 0 };
    },
  };
  return { seam, calls };
}

async function openMemory(seams: Seams, client = new FixtureClient()) {
  render(<App client={client} seams={seams} initialSection="memory" />);
  const card = (name: string) => screen.findByRole("heading", { name }).then((heading) => heading.closest("section")!);
  return {
    remember: await card("What your agents remember"),
    canDo: await card("What your agents can do"),
    did: await card("What your agents did"),
  };
}

describe("What your agents remember", () => {
  it("confirms, fixes and forgets a guess through the routes, and says what became of each", async () => {
    const { seam, calls } = memory();
    const { remember } = await openMemory({ ...fixtureSeams(), memory: seam });
    await within(remember).findByText("You bill [CLIENT] at [RATE] an hour.");
    expect(within(remember).getByText("You said this")).toBeTruthy();
    expect(within(remember).getAllByText("A guess")).toHaveLength(2);
    expect(within(remember).getAllByText("Finance noticed this")).toHaveLength(2);

    const guess = (text: string) => within(remember).getByText(text).closest("li")!;
    await userEvent.click(within(guess("Your accountant is [NAME].")).getByRole("button", { name: "That's right" }));
    await within(remember).findByText("You confirmed this");

    await userEvent.click(
      within(guess("You send invoices on the last working day.")).getByRole("button", { name: "Fix it" }),
    );
    const field = within(remember).getByRole("textbox", { name: "Your words" });
    await userEvent.clear(field);
    await userEvent.type(field, "You send invoices on the first working day.");
    // Esc never sends; Enter in the field saves it.
    await userEvent.keyboard("{Enter}");
    await within(remember).findByText("You send invoices on the first working day.");
    expect(within(remember).getByRole("status").textContent).toBe("Fixed. Your agents use your words from now on.");

    const confirmed = within(remember).getByText("Your accountant is [NAME].").closest("li")!;
    expect(within(confirmed).queryByRole("button", { name: "Forget it" })).toBeNull();
    expect(calls).toEqual(["confirm f2", "fix f3 You send invoices on the first working day."]);
  });

  it("forgets a guess, and it is gone from the list", async () => {
    const { seam, calls } = memory();
    const { remember } = await openMemory({ ...fixtureSeams(), memory: seam });
    const guess = (await within(remember).findByText("Your accountant is [NAME].")).closest("li")!;
    await userEvent.click(within(guess).getByRole("button", { name: "Forget it" }));
    await waitFor(() => expect(within(remember).queryByText("Your accountant is [NAME].")).toBeNull());
    expect(within(remember).getByRole("status").textContent).toBe("Forgotten. Your agents won't use it from their next step.");
    expect(calls).toEqual(["forget f2"]);
  });

  it("undoes an overnight tidy-up and says what came back", async () => {
    const { seam, calls } = memory();
    const { remember } = await openMemory({ ...fixtureSeams(), memory: seam });
    await within(remember).findByText("Overnight, your agents merged two notes into one.");
    await userEvent.click(within(remember).getByRole("button", { name: "Undo" }));
    await within(remember).findByText("Undone. Two notes are back as they were.");
    expect(within(remember).queryByRole("button", { name: "Undo" })).toBeNull();
    expect(within(remember).getByText("Your accountant's email is [EMAIL].")).toBeTruthy();
    expect(calls).toEqual(["undo tick-1"]);
  });

  it("says when the agents keep no memory, never that they remember nothing", async () => {
    const { remember } = await openMemory({ ...fixtureSeams(), memory: { ...memory().seam, facts: async () => undefined } });
    await within(remember).findByText("Your agents keep no memory between conversations here, so there is nothing to show.");
  });

  it("escapes a fact's text, which a model wrote (S-I9)", async () => {
    const seam = memory().seam;
    const { remember } = await openMemory({
      ...fixtureSeams(),
      memory: { ...seam, facts: async () => ({ facts: [{ id: "x", text: "<img src=x onerror=alert(1)>", provenance: "aGuess" }] }) },
    });
    await within(remember).findByText("<img src=x onerror=alert(1)>");
    expect(remember.querySelector("img")).toBeNull();
  });
});

describe("the memory in the home's words", () => {
  const tick = (overrides: Partial<KernelTidyUp>): KernelTidyUp => ({
    id: "t",
    area: "default",
    at: new Date(2026, 9, 25, 3, 12).toISOString(),
    finishedAt: new Date(2026, 9, 25, 3, 13).toISOString(),
    merged: 1,
    superseded: 2,
    forgotten: 0,
    distilled: 0,
    ...overrides,
  });
  const morning = new Date(2026, 9, 25, 8, 0);

  it("says what a tidy-up did, in which area, and when", () => {
    expect(tidyUpText(tick({}), morning)).toBe("Overnight, your agents merged two notes into one.");
    expect(tidyUpText(tick({ area: "work", forgotten: 1, distilled: 3 }), morning)).toBe(
      "Overnight, your agents at work merged two notes into one, forgot one note and learned three things from your conversations.",
    );
    expect(tidyUpText(tick({ at: new Date(2026, 9, 25, 14, 2).toISOString() }), new Date(2026, 9, 25, 15, 0))).toBe(
      "Today at 14:02, your agents merged two notes into one.",
    );
    expect(tidyUpText(tick({ at: new Date(2026, 9, 20, 3, 0).toISOString() }), morning)).toBe(
      "On 20 October, your agents merged two notes into one.",
    );
  });

  it("offers Undo for the latest finished tidy-up that changed something and was not undone", () => {
    const read = memoryOf(
      {
        facts: [
          { id: "f1", text: "A", origin: "youSaidThis", learnedAt: "" },
          { id: "f2", text: "B", origin: "guess", noticedBy: "work", learnedAt: "" },
          { id: "f3", text: "C", origin: "youConfirmedThis", learnedAt: "" },
        ],
        tidyUps: [
          tick({ id: "running", finishedAt: undefined }),
          tick({ id: "undone", undoneAt: new Date().toISOString() }),
          tick({ id: "empty", merged: 0, superseded: 0 }),
          tick({ id: "this" }),
        ],
      },
      morning,
    );
    expect(read.tidyUp?.id).toBe("this");
    expect(read.facts.map((fact) => [fact.provenance, fact.noticedBy])).toEqual([
      ["youSaidThis", undefined],
      ["aGuess", "Work"],
      ["youConfirmedThis", undefined],
    ]);
    expect(factOf({ id: "g", text: "x", origin: "somethingNew", learnedAt: "" }).provenance).toBe("aGuess");
  });

  it("says what an undo did, and keeps what the person changed since", () => {
    expect(undoText({ restored: 2, forgotten: 1, kept: 0 })).toBe("Undone. Two notes are back as they were.");
    expect(undoText({ restored: 1, forgotten: 1, kept: 1 })).toBe(
      "Undone. One note is back as it was. One note you changed since stays as you left it.",
    );
    expect(undoText()).toBe("Undone.");
  });
});

describe("Allowed without asking", () => {
  it("lists each rule as a sentence with its end date; Extend moves it later and shows the change", async () => {
    const client = new FixtureClient();
    const { canDo } = await openMemory(fixtureSeams(), client);
    const accountant = (await within(canDo).findByText("Finance can use send_email to [accountant email].")).closest("li")!;
    expect(within(accountant).getByText("Until 30 November")).toBeTruthy();
    const tests = within(canDo).getByText("Code can run commands that start with cargo test.").closest("li")!;
    expect(within(tests).getByText("No end date")).toBeTruthy();
    // A rule without an end date has nothing to extend.
    expect(within(tests).queryByRole("button", { name: "Extend" })).toBeNull();

    await userEvent.click(within(accountant).getByRole("button", { name: "Extend" }));
    const date = within(accountant).getByLabelText("Until the end of") as HTMLInputElement;
    // Thirty days after its end are offered, and nothing before the day after it.
    expect(date.value).toMatch(/^\d{4}-12-30$/);
    expect(date.min).toMatch(/^\d{4}-12-01$/);
    // Esc closes it without changing anything.
    await userEvent.keyboard("{Escape}");
    expect(within(accountant).queryByLabelText("Until the end of")).toBeNull();

    await userEvent.click(within(accountant).getByRole("button", { name: "Extend" }));
    const field = within(accountant).getByLabelText("Until the end of");
    await userEvent.clear(field);
    await userEvent.type(field, `${new Date().getFullYear()}-12-24`);
    await userEvent.click(within(accountant).getByRole("button", { name: "Extend" }));
    const row = (await within(canDo).findByText("Until 24 December")).closest("li")!;
    expect(within(row).getByRole("status").textContent).toBe("Was until 30 November. Now until 24 December.");
    const rules = (await client.rules()) as StandingRule[];
    const extended = new Date(rules.find((rule) => rule.id === "rule_accountant")!.expiresAt!);
    expect([extended.getMonth(), extended.getDate(), extended.getHours(), extended.getMinutes()]).toEqual([11, 24, 23, 59]);
  });

  it("says a rule has ended, and Stop allowing removes it", async () => {
    const client = new FixtureClient();
    const lapsed: StandingRule = {
      id: "rule_lapsed",
      tool: "bash",
      label: "bash: npm run lint …",
      decision: { kind: "allow" },
      createdAt: new Date(2026, 8, 1).toISOString(),
      expiresAt: new Date(Date.now() - 86_400_000).toISOString(),
    };
    (client as unknown as { ruleList: StandingRule[] }).ruleList.push(lapsed);
    expect(ended(lapsed, new Date())).toBe(true);
    const { canDo } = await openMemory(fixtureSeams(), client);
    const row = (await within(canDo).findByText("Your agents can run commands that start with npm run lint.")).closest("li")!;
    expect(within(row).getByText(/^Ended /)).toBeTruthy();
    expect(within(row).getByRole("button", { name: "Extend" })).toBeTruthy();
    await userEvent.click(within(row).getByRole("button", { name: "Stop allowing" }));
    await waitFor(() => expect(within(canDo).queryByText("Your agents can run commands that start with npm run lint.")).toBeNull());
  });

  it("words each kind of rule the harness makes", () => {
    const rule = (label: string, tool: string, agent?: string): StandingRule => ({
      id: "r",
      tool,
      agent,
      label,
      decision: { kind: "allow" },
      createdAt: "",
    });
    expect(ruleSentence(rule("write_file: within src/ (this session)", "write_file"), rows)).toBe(
      "Your agents can use write_file within src/, in one conversation.",
    );
    expect(ruleSentence(rule("web_fetch: any page on example.com", "web_fetch", "urn:adk:agent:home/finance"), rows)).toBe(
      "Finance can use web_fetch on any page of example.com.",
    );
    expect(ruleSentence(rule("web_search: searches restricted to docs.rs", "web_search"), rows)).toBe(
      "Your agents can search docs.rs.",
    );
    expect(ruleSentence(rule("record_note", "record_note"), rows)).toBe("Your agents can use record_note.");
    expect(ruleSentence(rule("record_note", "record_note", "urn:adk:agent:home/*"), rows)).toBe("Your agents can use record_note.");
  });
});

describe("What your agents did", () => {
  it("checks the record and lists the latest actions as sentences, without the kernel's housekeeping", async () => {
    const { did } = await openMemory(fixtureSeams());
    await within(did).findByText("Every action is on record");
    expect(within(did).getByText(/^All 1,284 of them, and nothing has been changed or deleted\. Checked just now\.$/)).toBeTruthy();
    const lines = within(did).getAllByRole("listitem").filter((item) => item.className === "record-line");
    expect(lines.map((line) => line.querySelector("span")?.textContent)).toEqual([
      "You said yes to send_email, from Telegram.",
      "You said yes to run_tests, from your phone.",
      "You said no to a suggestion.",
    ]);
  });

  it("says when something on record was changed or deleted, with what the check found", async () => {
    const seams = fixtureSeams();
    seams.record = {
      ...seams.record!,
      check: async () => ({ intact: false, count: 12, checkedAt: new Date().toISOString(), problem: "entry 7 does not match its checkpoint" }),
    };
    const { did } = await openMemory(seams);
    await within(did).findByText("Something on record was changed or deleted.");
    expect(within(did).queryByText("Every action is on record")).toBeNull();
    expect(within(did).getByText("entry 7 does not match its checkpoint")).toBeTruthy();
  });

  it("says which records are not signed yet, never that one was changed (spec 3.3 §5.1)", async () => {
    const seams = fixtureSeams();
    seams.record = {
      ...seams.record!,
      check: async () => ({ intact: false, count: 12, checkedAt: new Date().toISOString(), problem: "[unanchored] no checkpoint signs these records", unsigned: 3 }),
    };
    const { did } = await openMemory(seams);
    await within(did).findByText("Every action is on record");
    expect(within(did).queryByText("Something on record was changed or deleted.")).toBeNull();
    expect(
      within(did).getByText(
        "All 12 of them. The first 9 are signed and unchanged; the latest 3 are not signed yet, so the check can't vouch for them until the record is next signed. Checked just now.",
      ),
    ).toBeTruthy();
  });

  it("reads the activity ledger where the kernel keeps no signed record", async () => {
    const seams = fixtureSeams();
    seams.record = { check: async () => undefined, lines: async () => [] };
    const { did } = await openMemory(seams);
    await within(did).findByText(/Household will retry when you say so\./);
    expect(within(did).queryByText("Every action is on record")).toBeNull();
  });

  it("words each action a person or an agent takes, and leaves housekeeping out", () => {
    const line = (fields: Partial<KernelRecordLine>): KernelRecordLine => ({ seq: 1, at: "", action: "", ...fields });
    const said = (fields: Partial<KernelRecordLine>) => recordSentence(line(fields), rows);
    expect(said({ action: "surface.decide", outcome: "decided", decision: "rejected", tool: "pay", how: "pointer" })).toBe(
      "You said no to pay, in the home.",
    );
    expect(said({ action: "surface.decide", outcome: "conflict", decision: "approved", tool: "pay" })).toBeUndefined();
    expect(said({ action: "surface.decide", outcome: "decided", decision: "approved", tool: "pay", how: "touch", channel: "Ana's phone" })).toBe(
      "You said yes to pay, from Ana's phone.",
    );
    expect(said({ action: "kernel.control", kernelAction: "approvals.decide", decision: "approved", tool: "send_email", channel: "Ana's phone" })).toBe(
      "You said yes to send_email, from Ana's phone.",
    );
    expect(said({ action: "harness.rule.created", tool: "bash" })).toBe("You allowed bash without asking.");
    expect(said({ action: "harness.rule.revoked", tool: "bash" })).toBe("You stopped allowing bash without asking.");
    expect(said({ action: "harness.rule.extended", tool: "bash" })).toBe("You extended how long bash is allowed without asking.");
    expect(said({ action: "kernel.control", kernelAction: "memory.forget", target: "f1" })).toBe("You made your agents forget a note.");
    expect(said({ action: "kernel.control", kernelAction: "settings.quiet_time" })).toBe("You changed quiet time.");
    expect(said({ action: "kernel.channel", kernelAction: "stop", how: "chat", channel: "telegram" })).toBe(
      "You stopped all agents, from Telegram.",
    );
    expect(said({ action: "kernel.control", kernelAction: "pairing.claim" })).toBe("A phone or app was paired.");
    expect(said({ action: "surface.stop_all", how: "keyboard" })).toBe("You stopped all agents, from the terminal.");
    expect(said({ action: "write_file", tool: "write_file", agent: "urn:adk:agent:home/finance", outcome: "succeeded" })).toBe(
      "Finance used write_file.",
    );
    expect(said({ action: "write_file", tool: "write_file", agent: "urn:adk:agent:adk-rust/adk-rust", decision: "allowed" })).toBe(
      "Adk rust used write_file.",
    );
    expect(said({ action: "harness.rule.matched", tool: "write_file", agent: "urn:adk:agent:home/code" })).toBeUndefined();
    expect(said({ action: "bash", tool: "bash", agent: "urn:adk:agent:home/code", outcome: "failed" })).toBe(
      "Code tried bash, and it failed.",
    );
    // Housekeeping, a message sent and the approval service's copy of an answer are left out.
    for (const fields of [
      { action: "kernel.control", kernelAction: "spawn" },
      { action: "kernel.control" },
      { action: "surface.send" },
      { action: "send_email", decision: "approved" },
      { action: "governance.freeze" },
    ]) {
      expect(said(fields)).toBeUndefined();
    }
    const shown = recentLines(recordLines(), rows, new Date(), 2);
    expect(shown.map((entry) => entry.seq)).toEqual([1284, 1282]);
  });
});

describe("each area's agents and what they spent", () => {
  it("lists every agent with what it spent today, adding up to the area's spent today", async () => {
    const { canDo } = await openMemory(fixtureSeams());
    const roster = await within(canDo).findByRole("region", { name: "What each agent spent today" });
    const entries = within(roster).getAllByRole("listitem").map((item) => item.textContent);
    expect(entries.slice(0, 3)).toEqual(["FFinanceWaiting for you$1.12 today", "{ }CodeWaiting for you$0.42 today", "RResearchDone$0.30 today"]);
    // $1.12 + $0.42 + $0.30 is the $1.84 the area spent today.
    expect(within(canDo).getByText("$1.84 spent today")).toBeTruthy();
  });

  it("keeps each area's agents to that area and names a per-area target as the roster's agent", () => {
    const spending = [
      { agent: "adk-rust@work", area: "work", spentToday: 500_000 },
      { agent: "harness.goal@work", area: "work", spentToday: 100_000 },
      { agent: "adk-rust@home", area: "home", spentToday: 200_000 },
    ];
    const roster: AgentRow[] = [
      { agent: { name: "adk-rust", displayName: "ADK-Rust", avatar: { monogram: "A", color: "#C03A00" } }, status: "working", statusText: "Working" },
    ];
    const work = areaAgents("work", spending, roster, false);
    expect(work.map((entry) => [entry.agent.displayName, entry.spentToday])).toEqual([
      ["ADK-Rust", 500_000],
      ["Harness goal", 100_000],
    ]);
    expect(work.reduce((sum, entry) => sum + entry.spentToday, 0)).toBe(600_000);
    expect(areaAgents("home", spending, roster, false).map((entry) => entry.spentToday)).toEqual([200_000]);
    // The kernel's own work is one upkeep row, listed only when it spent something.
    const upkeep = areaAgents("work", [...spending, { agent: "kernel.proc", area: "work", spentToday: 40_000 }], roster, false);
    expect(upkeep.map((entry) => [entry.agent.displayName, entry.spentToday])).toContainEqual([
      "Upkeep: tidying notes and checking work",
      40_000,
    ]);
    expect(areaAgents("work", [{ agent: "kernel.proc", area: "work", spentToday: 0 }], roster, false)).toEqual([]);
    // A kernel without areas lists the whole roster, at nothing spent where nothing was.
    expect(areaAgents("default", [], roster, true).map((entry) => [entry.agent.displayName, entry.spentToday])).toEqual([["ADK-Rust", 0]]);
  });
});

describe("routines on some days of the week", () => {
  it("names the next day a weekday routine runs, so Saturday's next is Monday", () => {
    const weekdays = { cron: { expr: "0 9 * * 1-5" } };
    const saturday = new Date(Date.UTC(2026, 9, 24, 10, 0));
    expect(nextFire(weekdays, saturday)?.toISOString()).toBe("2026-10-26T09:00:00.000Z");
    const friday = new Date(Date.UTC(2026, 9, 23, 8, 0));
    expect(nextFire(weekdays, friday)?.toISOString()).toBe("2026-10-23T09:00:00.000Z");
    expect(nextFire({ cron: { expr: "30 7 * * SAT,SUN" } }, friday)?.toISOString()).toBe("2026-10-24T07:30:00.000Z");
    expect(nextFire({ cron: { expr: "0 9 1 * *" } }, friday)).toBeUndefined();
    expect([...cronWeekdays("5-7")!].sort()).toEqual([0, 5, 6]);
    expect(cronWeekdays("*/2")).toBeUndefined();
  });
});

describe("quiet time in a time zone", () => {
  it("reads a window kept in an IANA zone at that zone's offset, daylight saving time included", () => {
    expect(zoneOffset("Europe/London", new Date(Date.UTC(2026, 6, 1, 12)))).toBe(60);
    expect(zoneOffset("Europe/London", new Date(Date.UTC(2026, 11, 1, 12)))).toBe(0);
    expect(zoneOffset("Asia/Kolkata", new Date(Date.UTC(2026, 0, 1)))).toBe(330);
    expect(zoneOffset("Not/AZone")).toBeUndefined();
    // Tokyo's 22:00–07:00 is 16:00–01:00 in Nairobi; the zone wins over an offset beside it.
    expect(onThisDevice({ start: "22:00", end: "07:00", timeZone: "Asia/Tokyo", utcOffsetMinutes: 0 }, 0, 180)).toEqual({
      start: "16:00",
      end: "01:00",
      utcOffsetMinutes: 180,
    });
  });
});

describe("the HTTP client's memory, record, rule and spending routes", () => {
  it("reads and corrects the memory, reads the record and its check, extends a rule and sums each agent's spend", async () => {
    const seen: Array<[string, string, unknown]> = [];
    vi.stubGlobal(
      "fetch",
      vi.fn(async (url: string, init?: RequestInit) => {
        seen.push([init?.method ?? "GET", url, init?.body ? JSON.parse(String(init.body)) : undefined]);
        const body: Record<string, unknown> = {
          "/api/kernel/memory": {
            facts: [{ id: "f1", text: "A", origin: "guess", noticedBy: "work", learnedAt: "" }],
            tidyUps: [{ id: "t1", area: "default", at: new Date().toISOString(), finishedAt: new Date().toISOString(), merged: 1, superseded: 2, forgotten: 0, distilled: 0 }],
          },
          "/api/kernel/memory/undo": { id: "t1", restored: ["a", "b"], forgotten: ["c"], kept: [] },
          "/api/kernel/record/check": { valid: true, entries: 7, checkedAt: "2026-10-25T07:00:00Z", report: {} },
          "/api/kernel/record?limit=200": { lines: [{ seq: 7, at: "", action: "surface.decide" }] },
          "/api/rules/r%201/extend": { rule: { id: "r 1", expiresAt: "2026-12-24T20:59:00.000Z" } },
          "/api/kernel/processes": {
            processes: [
              { run: { runId: "1", target: "finance", state: "completed" }, meta: { pid: "1", partition: "work" }, view: { spendToday: 300 } },
              { run: { runId: "2", target: "finance", state: "running" }, meta: { pid: "2", partition: "work" }, view: { spendToday: 200 } },
              { run: { runId: "3", target: "finance", state: "waiting" }, meta: { pid: "3", partition: "home" }, view: { spendToday: 50 } },
              { run: { runId: "4", target: "code", state: "completed" }, meta: { pid: "4" } },
            ],
          },
        };
        return new Response(JSON.stringify(body[url] ?? {}), { status: 200 });
      }),
    );
    const client = new HttpClient();
    const memory = client.memory();
    const read = await memory.facts();
    expect(read?.facts).toEqual([{ id: "f1", text: "A", provenance: "aGuess", noticedBy: "Work" }]);
    expect(read?.tidyUp?.id).toBe("t1");
    await memory.confirm("f 1");
    await memory.fix("f1", "B");
    await memory.forget("f1");
    expect(await memory.undo("t1")).toEqual({ restored: 2, forgotten: 1, kept: 0 });
    const record = client.record();
    expect(await record.check()).toEqual({ intact: true, count: 7, checkedAt: "2026-10-25T07:00:00Z", problem: undefined, unsigned: undefined });
    expect(await record.lines(200)).toEqual([{ seq: 7, at: "", action: "surface.decide" }]);
    expect((await client.extendRule("r 1", "2026-12-24T20:59:00.000Z")).expiresAt).toBe("2026-12-24T20:59:00.000Z");
    expect(await client.roster().spending()).toEqual([
      { agent: "finance", area: "work", spentToday: 500 },
      { agent: "finance", area: "home", spentToday: 50 },
      { agent: "code", area: "default", spentToday: 0 },
    ]);
    expect(seen).toEqual([
      ["GET", "/api/kernel/memory", undefined],
      ["POST", "/api/kernel/memory/facts/f%201/confirm", {}],
      ["POST", "/api/kernel/memory/facts/f1/fix", { text: "B" }],
      ["POST", "/api/kernel/memory/facts/f1/forget", {}],
      ["POST", "/api/kernel/memory/undo", { tidyUp: "t1" }],
      ["GET", "/api/kernel/record/check", undefined],
      ["GET", "/api/kernel/record?limit=200", undefined],
      ["POST", "/api/rules/r%201/extend", { expiresAt: "2026-12-24T20:59:00.000Z" }],
      ["GET", "/api/kernel/processes", undefined],
    ]);
  });

  it("reads no memory and no record from a kernel set up without them", async () => {
    vi.stubGlobal(
      "fetch",
      vi.fn(
        async () =>
          new Response(JSON.stringify({ error: { code: "kernel.config", message: "this kernel shows no memory" } }), { status: 400 }),
      ),
    );
    const client = new HttpClient();
    expect(await client.memory().facts()).toBeUndefined();
    expect(await client.record().check()).toBeUndefined();
    expect(await client.record().lines(5)).toEqual([]);
  });
});

describe("Coming up", () => {
  it("names the day of what is planned beyond today", async () => {
    const { whenAhead } = await import("../src/format");
    const saturday = new Date(2026, 9, 24, 10, 0);
    expect(whenAhead(new Date(2026, 9, 24, 18, 30), saturday)).toBe("18:30");
    expect(whenAhead(new Date(2026, 9, 26, 9, 0), saturday)).toBe("Mon 09:00");
    expect(whenAhead(new Date(2026, 10, 2, 9, 0), saturday)).toBe("2 November, 09:00");
  });
});

describe("the record check's unsigned tail", () => {
  it("is read from the verifier's report only when nothing but unsigned records failed", async () => {
    const answer = (report: unknown) =>
      vi.fn(async () => new Response(JSON.stringify({ valid: false, entries: 8, checkedAt: "", problem: "p", report }), { status: 200 }));
    vi.stubGlobal("fetch", answer({ unsignedTail: 8, failures: [{ kind: "unanchored" }] }));
    expect((await new HttpClient().record().check())?.unsigned).toBe(8);
    vi.stubGlobal("fetch", answer({ unsignedTail: 8, failures: [{ kind: "unanchored" }, { kind: "hash_mismatch" }] }));
    expect((await new HttpClient().record().check())?.unsigned).toBeUndefined();
  });
});

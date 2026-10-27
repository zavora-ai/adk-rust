// Screen actions on the home (spec 3.3 §2 item 7, §5.1, §5.6): Snooze on a Coming up row that
// may be snoozed and on no other, only from the person's own input and shown done only once the
// server answered; "On your screens" in Memory & rules; the record's sentences; a spoken
// snooze in the voice view; and nothing of it where the server has no screen actions.
import { act, render, screen, waitFor, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, describe, expect, it, vi } from "vitest";
import { App } from "../src/App";
import { ApiError } from "../src/client";
import { FixtureClient, agents, fixtureSeams } from "../src/fixture/fixture";
import { VerbsFixture, VerbsHomeFixture, snoozeCard, verbsSeams } from "../src/fixture/verbs";
import { FakeDevices, VoiceFixtureClient } from "../src/fixture/voice";
import type { AgentRow } from "../src/generated/api";
import type { KernelRecordLine } from "../src/kernel";
import { recentLines, recordSentence } from "../src/record";
import { Voice } from "../src/screens/Voice";
import type { VerbsClient } from "../src/verbs/client";

/** Whether input counts as the person's own: the browser marks it trusted, which jsdom never does. */
const input = vi.hoisted(() => ({ trusted: false }));
vi.mock("../src/verbs/trust", () => ({ trusted: () => input.trusted }));

afterEach(() => {
  input.trusted = false;
  vi.restoreAllMocks();
});

function asThePerson() {
  input.trusted = true;
}

function comingUp() {
  return screen.getByRole("region", { name: "Coming up" });
}

describe("Snooze in Coming up", () => {
  it("offers an hour later and the same time tomorrow on an item that may move, and says so once done", async () => {
    asThePerson();
    const verbs = new VerbsFixture();
    const snooze = vi.spyOn(verbs, "snooze");
    render(<App client={new VerbsHomeFixture()} initialSection="threads" verbs={verbs} />);
    const button = await within(comingUp()).findByRole("button", { name: "Snooze News digest" });
    // Routines with no agenda item have no Snooze.
    expect(within(comingUp()).queryByRole("button", { name: /Snooze CI check/ })).toBeNull();
    await userEvent.click(button);
    const menu = within(comingUp()).getByRole("menu");
    const items = within(menu).getAllByRole("menuitem");
    expect(items.map((item) => item.textContent?.replace(/\d{2}:\d{2}/, "HH:MM"))).toEqual([
      "1 hour laterHH:MM",
      expect.stringMatching(/^Tomorrow, same time\w{3} HH:MM$/),
    ]);
    await userEvent.click(items[0]!);
    expect(snooze).toHaveBeenCalledTimes(1);
    const [item, choice, origin] = snooze.mock.calls[0]!;
    expect(item.id).toBe("agenda-7f3");
    expect(choice.label).toBe("1 hour later");
    expect(origin).toEqual({ kind: "pointer" });
    expect(await within(comingUp()).findByText("Snoozed by you · just now")).toBeTruthy();
  });

  it("does nothing on a script's click", async () => {
    const verbs = new VerbsFixture();
    const snooze = vi.spyOn(verbs, "snooze");
    render(<App client={new VerbsHomeFixture()} initialSection="threads" verbs={verbs} />);
    const button = await within(comingUp()).findByRole("button", { name: "Snooze News digest" });
    await userEvent.click(button);
    expect(within(comingUp()).queryByRole("menu")).toBeNull();
    expect(snooze).not.toHaveBeenCalled();
  });

  it("says why it can't, and shows nothing as done", async () => {
    asThePerson();
    render(<App client={new VerbsHomeFixture()} initialSection="threads" verbs={new VerbsFixture("cant")} />);
    await userEvent.click(await within(comingUp()).findByRole("button", { name: "Snooze News digest" }));
    await userEvent.click(within(within(comingUp()).getByRole("menu")).getAllByRole("menuitem")[0]!);
    expect(await within(comingUp()).findByText("Can't snooze: it has already started")).toBeTruthy();
    expect(within(comingUp()).queryByText("Snoozed by you · just now")).toBeNull();
  });

  it("is absent where the server has no screen actions", async () => {
    const absent: VerbsClient = {
      list: async () => undefined,
      snooze: async () => {
        throw new ApiError(404, "no route");
      },
    };
    render(<App client={new VerbsHomeFixture()} initialSection="threads" verbs={absent} />);
    await within(comingUp()).findByText("Research · News digest");
    expect(within(comingUp()).queryByRole("button", { name: /Snooze/ })).toBeNull();
  });
});

describe("an agent's request", () => {
  it("is the ordinary card: the kernel's words, Allow once · Edit · Deny, and the fixed line", async () => {
    const client = new VerbsHomeFixture();
    client.hold(snoozeCard());
    render(<App client={client} initialSection="threads" verbs={new VerbsFixture()} />);
    const card = await screen.findByRole("article", { name: /Move News digest to/ });
    const answers = within(card)
      .getAllByRole("button")
      .map((button) => button.textContent);
    expect(answers).toEqual(expect.arrayContaining(["Allow once", "Edit", "Deny"]));
    expect(answers).not.toContain("Always allow for this person");
    expect(within(card).getByText("Agents always ask before they change things on your screens.")).toBeTruthy();
    expect(within(card).getByText("You can change it back")).toBeTruthy();
  });
});

describe("Memory & rules", () => {
  it("lists the screen actions agents ask for, and that they always ask", async () => {
    render(
      <App
        client={new FixtureClient()}
        seams={verbsSeams(fixtureSeams())}
        initialSection="memory"
        verbs={new VerbsFixture()}
      />,
    );
    expect(await screen.findByText("On your screens")).toBeTruthy();
    expect(screen.getByText("Snooze something in Coming up")).toBeTruthy();
    expect(screen.getByText("Agents always ask you first")).toBeTruthy();
    expect(
      screen.getByText(
        "When an agent wants to change something on your screens, it asks you first. This can't be switched off.",
      ),
    ).toBeTruthy();
    // The read never asks: it is not listed.
    expect(screen.queryByText(/List what is coming up/)).toBeNull();
  });

  it("shows no section where the server has none", async () => {
    render(<App client={new FixtureClient()} seams={fixtureSeams()} initialSection="memory" />);
    await screen.findByText("What your agents can do");
    expect(screen.queryByText("On your screens")).toBeNull();
  });
});

describe("the record", () => {
  const rows: AgentRow[] = [{ agent: agents.finance, status: "working", statusText: "Working", sessionId: "s-finance" }];
  const line = (rest: Partial<KernelRecordLine>): KernelRecordLine => ({
    seq: 1,
    at: new Date().toISOString(),
    action: "surface.verb",
    tool: "snooze",
    target: "News digest",
    outcome: "done",
    ...rest,
  });

  it("says who snoozed what, and where", () => {
    expect(
      [
        line({ surface: "terminal", how: "keyboard" }),
        line({ surface: "web", how: "pointer" }),
        line({ surface: "voice", how: "voice" }),
        line({ agent: "urn:adk:agent:home/finance", decision: "approved", surface: "phone", how: "touch" }),
        line({ agent: "urn:adk:agent:home/finance", outcome: "refused" }),
        line({
          action: "surface.decide",
          tool: "verb_snooze",
          agent: "urn:adk:agent:home/finance",
          decision: "rejected",
          outcome: "decided",
          target: "apr_1",
        }),
        line({ action: "surface.decide", tool: "verb_snooze", agent: "urn:adk:agent:home/finance", decision: "approved", outcome: "decided" }),
      ].map((each) => recordSentence(each, rows)),
    ).toEqual([
      "You snoozed News digest in the terminal.",
      "You snoozed News digest in the home app.",
      "You snoozed News digest by voice.",
      "Finance asked to snooze News digest; you said yes on your phone.",
      "Finance asked to snooze News digest; it had moved, so nothing changed.",
      "Finance asked to snooze something in Coming up; you said no. Nothing changed.",
      undefined,
    ]);
    expect(recentLines([line({ surface: "voice", how: "voice" })], rows)).toHaveLength(1);
  });
});

describe("a spoken snooze", () => {
  async function connected(screenActions = true) {
    const devices = new FakeDevices();
    render(<Voice client={new VoiceFixtureClient()} devices={devices} onEnd={vi.fn()} screenActions={screenActions} />);
    await waitFor(() => expect(devices.sent[0]).toEqual({ text: { type: "hello", ticket: "fixture" } }));
    act(() => devices.say({ type: "ready", sessionId: "voice-1", inputRate: 16_000, outputRate: 24_000 }));
    return devices;
  }

  it("counts down the engine's window under the kernel's words, and Cancel withdraws it", async () => {
    asThePerson();
    const devices = await connected();
    act(() => {
      devices.say({ type: "heard", text: "Snooze news digest." });
      devices.say({
        type: "pendingAction",
        actionId: "act-1",
        cancelUntil: new Date(Date.now() + 3_000).toISOString(),
        text: "Snoozing in 3 s",
        heading: "Move News digest to 19:00",
      });
    });
    expect(await screen.findByRole("heading", { name: "Move News digest to 19:00" })).toBeTruthy();
    expect(screen.getByText("Heard “Snooze news digest.”")).toBeTruthy();
    expect(screen.getByText("Snoozing in 3 s")).toBeTruthy();
    const cancels = screen.getAllByRole("button", { name: "Cancel" });
    await userEvent.click(cancels[cancels.length - 1]!);
    expect(devices.sent.flatMap((sent) => ("text" in sent ? [sent.text] : []))).toContainEqual({
      type: "cancelAction",
      actionId: "act-1",
    });
    act(() =>
      devices.say({
        type: "actionOutcome",
        actionId: "act-1",
        done: false,
        text: "Cancelled · nothing changed",
        heading: "Move News digest to 19:00",
      }),
    );
    expect(await screen.findByText("Cancelled · nothing changed")).toBeTruthy();
    expect(screen.getByText(/By voice you can snooze what's coming up/)).toBeTruthy();
  });

  it("says when the words match several things, and runs nothing", async () => {
    const devices = await connected();
    act(() =>
      devices.say({
        type: "actionOutcome",
        done: false,
        text: "Two things match “digest” — say the full name or use the screen",
      }),
    );
    expect(await screen.findByText("Two things match “digest” — say the full name or use the screen")).toBeTruthy();
    expect(screen.queryByRole("button", { name: "Cancel" })).toBeNull();
  });

  it("keeps the old note where the home takes no screen actions", async () => {
    await connected(false);
    expect(screen.queryByText(/snooze what's coming up/)).toBeNull();
  });
});

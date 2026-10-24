// A thread follows one conversation (spec 3.3 §5.1, §6, §7.4): a decision opens in its own
// session's thread, the thread stays on its conversation whatever the roster row says later,
// what waits behind an open decision is read back from the server, and a decided card stays.
import { render, screen, waitFor, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { describe, expect, it } from "vitest";
import { App } from "../src/App";
import { Thread } from "../src/components/Thread";
import type { DecisionCard, FeedEntry } from "../src/generated/api";
import { ApiError } from "../src/client";
import { FixtureClient, invoiceCard } from "../src/fixture/fixture";

/** A second decision of Finance, in a session the Finance row does not name. */
function payrollCard(): DecisionCard {
  return { ...invoiceCard(), id: "apr_payroll", sessionId: "s-finance-payroll", heading: "Run the October payroll" };
}

describe("a thread", () => {
  it("opens a decision of another session of the same agent in that session, where it can be answered", async () => {
    const client = new FixtureClient();
    client.hold(payrollCard());
    render(<App client={client} initialSection="threads" />);
    const thread = screen.getByRole("main");
    await within(thread).findByRole("article", { name: "Send the October invoice to [CLIENT]" });

    const list = screen.getByRole("region", { name: /^Decisions/ });
    await userEvent.click(await within(list).findByRole("button", { name: /Run the October payroll/ }));
    const card = await within(thread).findByRole("article", { name: "Run the October payroll" });
    await userEvent.click(within(card).getByRole("button", { name: "Allow once" }));
    await within(card).findByText("You said yes in the home app at 07:40");
    expect(client.taken.commands.at(-1)?.command).toMatchObject({ type: "decide", decisionId: "apr_payroll" });
  });

  it("stays on its conversation when the roster row stops naming it", async () => {
    const client = new FixtureClient();
    render(<App client={client} initialSection="threads" />);
    const thread = screen.getByRole("main");
    const card = await within(thread).findByRole("article", { name: "Send the October invoice to [CLIENT]" });
    // The turn ends: the row of an idle agent names no session, as `adk-rust serve` reports.
    client.sessionOf.finance = null;
    await userEvent.click(within(card).getByRole("button", { name: "Allow once" }));
    await within(thread).findByText("You said yes in the home app at 07:40");
    await waitFor(async () => expect((await client.agents()).find((row) => row.agent.name === "finance")?.sessionId).toBeNull());
    await new Promise((resolve) => setTimeout(resolve, 100));
    expect(within(thread).getByText(/The October invoice for \[CLIENT\] is ready/)).toBeTruthy();
    await userEvent.type(within(thread).getByRole("textbox", { name: "Message Finance" }), "thanks{Enter}");
    await waitFor(() => expect(client.taken.commands.at(-1)?.command).toEqual({ type: "send", sessionId: "s-finance", text: "thanks" }));
  });

  it("keeps a new conversation when the person leaves Threads and comes back", async () => {
    const client = new FixtureClient();
    client.sessionOf.research = null;
    render(<App client={client} initialSection="threads" />);
    const roster = await screen.findByRole("navigation", { name: "Agents" });
    await userEvent.click(await within(roster).findByRole("button", { name: /Research/ }));
    const thread = screen.getByRole("main");
    await userEvent.type(within(thread).getByRole("textbox", { name: "Message Research" }), "find the paper{Enter}");
    await waitFor(() => expect(client.taken.commands.length).toBe(1));

    await userEvent.click(screen.getByRole("link", { name: "Today" }));
    await screen.findByRole("heading", { name: /While you were away/ });
    await userEvent.click(screen.getByRole("link", { name: /Threads/ }));
    await userEvent.type(within(screen.getByRole("main")).getByRole("textbox", { name: "Message Research" }), "and the data{Enter}");
    await waitFor(() => expect(client.taken.commands.length).toBe(2));
    const [first, second] = client.taken.commands.map(({ command }) => command);
    expect(first.type === "send" && second.type === "send" && first.sessionId === second.sessionId).toBe(true);
  });

  it("does not move to another agent that starts waiting once one is shown", async () => {
    const client = new FixtureClient();
    render(<App client={client} initialSection="threads" />);
    const thread = screen.getByRole("main");
    await within(thread).findByRole("heading", { name: "Finance" });
    // Finance's decision is answered elsewhere; Code still waits.
    client.elsewhere.add("apr_invoice");
    await userEvent.click(within(thread).getByRole("button", { name: "Deny" }));
    await within(thread).findByText("You said yes in Telegram at 07:31");
    expect(within(thread).getByRole("heading", { name: "Finance" })).toBeTruthy();
  });

  it("shows what waits behind an open decision after the person switches threads and back", async () => {
    const client = new FixtureClient();
    render(<App client={client} initialSection="threads" />);
    const thread = screen.getByRole("main");
    await within(thread).findByRole("article", { name: "Send the October invoice to [CLIENT]" });
    await userEvent.type(within(thread).getByRole("textbox", { name: "Message Finance" }), "also attach the timesheet{Enter}");
    await within(thread).findByText("also attach the timesheet");

    const roster = screen.getByRole("navigation", { name: "Agents" });
    await userEvent.click(within(roster).getByRole("button", { name: /Research/ }));
    await within(thread).findByRole("heading", { name: "Research" });
    expect(within(thread).queryByText("also attach the timesheet")).toBeNull();
    await userEvent.click(within(roster).getByRole("button", { name: /Finance/ }));
    await within(thread).findByRole("article", { name: "Send the October invoice to [CLIENT]" });
    expect(await within(thread).findByText("also attach the timesheet")).toBeTruthy();
    expect(client.taken.commands.filter(({ command }) => command.type === "send").length).toBe(1);
  });
});

describe("a decided card", () => {
  const entries: FeedEntry[] = [
    {
      seq: 1,
      at: new Date().toISOString(),
      atText: "18:00",
      items: [{ kind: "decision", tool: "send_email", text: "Waiting for you", decisionId: "apr_invoice" }],
    },
  ];

  it("stays in the thread, answered, when it leaves the open list and cannot be read again", async () => {
    const client = new FixtureClient();
    const send = (command: Parameters<FixtureClient["command"]>[0], origin: Parameters<FixtureClient["command"]>[1]) =>
      client.command(command, origin);
    const { rerender } = render(<Thread entries={entries} open={[invoiceCard()]} client={client} send={send} onSettled={() => {}} />);
    await userEvent.click(screen.getByRole("button", { name: "Allow once" }));
    await screen.findByText("You said yes in the home app at 07:40");
    client.decision = async () => {
      throw new ApiError(503, "unavailable");
    };
    rerender(<Thread entries={entries} open={[]} client={client} send={send} onSettled={() => {}} />);
    await new Promise((resolve) => setTimeout(resolve, 50));
    expect(screen.getByText("You said yes in the home app at 07:40")).toBeTruthy();
  });

  it("answered here stays in the thread when no line names it", async () => {
    const client = new FixtureClient();
    const send = (command: Parameters<FixtureClient["command"]>[0], origin: Parameters<FixtureClient["command"]>[1]) =>
      client.command(command, origin);
    const { rerender } = render(<Thread entries={[]} open={[invoiceCard()]} client={client} send={send} onSettled={() => {}} />);
    await userEvent.click(screen.getByRole("button", { name: "Allow once" }));
    await screen.findByText("You said yes in the home app at 07:40");
    rerender(<Thread entries={[]} open={[]} client={client} send={send} onSettled={() => {}} />);
    expect(screen.getByRole("article", { name: "Send the October invoice to [CLIENT]" })).toBeTruthy();
    expect(screen.getByText("You said yes in the home app at 07:40")).toBeTruthy();
  });
});

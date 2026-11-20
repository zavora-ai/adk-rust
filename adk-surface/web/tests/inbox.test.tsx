// Decisions, the inbox (spec 3.3 §5.1): everything waiting in one place, narrowed to who asks,
// the selected decision answered on its own card; a deep link still opens in its thread.
import { render, screen, waitFor, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { describe, expect, it, vi } from "vitest";
import { App } from "../src/App";
import { FixtureClient } from "../src/fixture/fixture";
import { decisionOf, pathOf, sectionOf } from "../src/route";

describe("the inbox's address", () => {
  it("is /ui/decisions; one decision's deep link still opens in its thread", () => {
    expect(pathOf("decisions")).toBe("/ui/decisions");
    expect(sectionOf("/ui/decisions")).toBe("decisions");
    expect(sectionOf("/ui/decisions/apr_invoice")).toBe("threads");
    expect(decisionOf("/ui/decisions/apr_invoice")).toBe("apr_invoice");
  });
});

describe("the inbox", () => {
  it("lists what waits, narrows it to who asks, and shows the selected card", async () => {
    render(<App client={new FixtureClient()} initialSection="decisions" />);
    expect(await screen.findByRole("heading", { name: "Decisions", level: 1 })).toBeTruthy();
    const list = await screen.findByRole("list", { name: "Waiting decisions" });
    await waitFor(() => expect(within(list).getAllByRole("button")).toHaveLength(2));
    expect(screen.getByText("2 waiting")).toBeTruthy();
    const detail = screen.getByRole("region", { name: "The selected decision" });
    expect(within(detail).getByRole("article", { name: "Send the October invoice to [CLIENT]" })).toBeTruthy();

    const filter = screen.getByRole("group", { name: "Filter by who asks" });
    await userEvent.click(within(filter).getByRole("button", { name: /Code/ }));
    expect(within(list).getAllByRole("button").map((item) => item.querySelector(".inbox-item-heading")?.textContent)).toEqual(["Publish a release"]);
    expect(within(detail).getByRole("article", { name: "Publish a release" })).toBeTruthy();
    await userEvent.click(within(filter).getByRole("button", { name: "Everyone" }));
    await userEvent.click(within(list).getByRole("button", { name: /Send the October invoice/ }));
    expect(within(list).getByRole("button", { name: /Send the October invoice/ }).getAttribute("aria-current")).toBe("true");
  });

  it("answers on the card, as anywhere else, and Enter on the list never decides", async () => {
    const client = new FixtureClient();
    const command = vi.spyOn(client, "command");
    render(<App client={client} initialSection="decisions" />);
    const list = await screen.findByRole("list", { name: "Waiting decisions" });
    const item = await within(list).findByRole("button", { name: /Send the October invoice/ });
    item.focus();
    await userEvent.keyboard("{Enter}");
    expect(command.mock.calls.some(([sent]) => sent.type === "decide")).toBe(false);
    const detail = screen.getByRole("region", { name: "The selected decision" });
    await userEvent.click(within(detail).getByRole("button", { name: "Allow once" }));
    await waitFor(() => expect(command.mock.calls.some(([sent]) => sent.type === "decide" && sent.answer === "allowOnce")).toBe(true));
  });

  it("opens a decision in its thread", async () => {
    render(<App client={new FixtureClient()} initialSection="decisions" />);
    const detail = await screen.findByRole("region", { name: "The selected decision" });
    await userEvent.click(await within(detail).findByRole("button", { name: "Open in its thread" }));
    expect(await screen.findByRole("heading", { name: "Finance" })).toBeTruthy();
  });
});

describe("thread history", () => {
  it("lists the agent's threads and opens an earlier one, with the way back", async () => {
    render(<App client={new FixtureClient()} initialSection="threads" />);
    const roster = await screen.findByRole("navigation", { name: "Agents" });
    await userEvent.click(within(roster).getByRole("button", { name: /Finance/ }));
    await userEvent.click(await screen.findByText("Thread history"));
    const list = await screen.findByRole("group", { name: "Thread history" });
    const threads = await within(list).findAllByRole("button");
    expect(threads.map((thread) => thread.textContent)).toEqual(["Yesterday 18:00this conversation", "31 Oct 16:40"]);
    await userEvent.click(within(list).getByRole("button", { name: "31 Oct 16:40" }));
    expect(await screen.findByRole("button", { name: "Back to your conversation" })).toBeTruthy();
  });
});

describe("stop the turn", () => {
  it("shows Stop beside Send while the agent works on the thread, and stops only that turn", async () => {
    const client = new FixtureClient();
    const command = vi.spyOn(client, "command").mockImplementation(async (sent) =>
      sent.type === "stopTurn"
        ? { type: "turnStopped", sessionId: sent.sessionId, stopped: true, text: "Stopped. Your agent waits for your next message." }
        : { type: "sent", sessionId: "s-chief" },
    );
    render(<App client={client} initialSection="threads" />);
    const roster = await screen.findByRole("navigation", { name: "Agents" });
    // Chief of staff is working; Finance waits for a decision and offers no Stop.
    await userEvent.click(within(roster).getByRole("button", { name: /Finance/ }));
    expect(screen.queryByRole("button", { name: "Stop the turn" })).toBeNull();
    await userEvent.click(within(roster).getByRole("button", { name: /Chief of staff/ }));
    await userEvent.click(await screen.findByRole("button", { name: "Stop the turn" }));
    await waitFor(() => expect(command).toHaveBeenCalledWith({ type: "stopTurn", sessionId: "s-chief" }, { kind: "pointer" }));
    expect(await screen.findByText("Stopped. Your agent waits for your next message.")).toBeTruthy();
  });
});

describe("how you answered this week", () => {
  it("counts the week's answers on Today, and where they were given", async () => {
    render(<App client={new FixtureClient()} initialSection="today" />);
    const card = await screen.findByRole("region", { name: "How you answered this week" });
    expect(within(card).getByText("12 answers: 9 allowed once, 2 always allowed, 1 denied")).toBeTruthy();
    expect(within(card).getAllByRole("listitem").map((item) => item.textContent)).toEqual(["Web home · 8", "Phone · 4"]);
  });

  it("is left out where the server keeps no count", async () => {
    const client = new FixtureClient();
    (client as { answers?: unknown }).answers = undefined;
    render(<App client={client} initialSection="today" />);
    await screen.findByRole("heading", { name: /While you were/ });
    expect(screen.queryByRole("region", { name: "How you answered this week" })).toBeNull();
  });
});

// Answered cards collapse to one line (spec 3.3 §5.1, §5.4, §5.5, §7.1, §7.4): a check, the
// heading and who answered; a click, Enter or Space opens and closes the line and never answers;
// on the tray the lines drop off five minutes after the answer and when a new decision arrives,
// three at most; in the home's thread a decided card keeps its place as its line; on the phone
// Decisions and the decision's own screen show the line.
import { act, render, screen, waitFor, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { App } from "../src/App";
import { DecisionCard } from "../src/components/DecisionCard";
import type { Command, CommandOutcome, DecisionCard as Card, Origin } from "../src/generated/api";
import { FixtureClient, invoiceCard } from "../src/fixture/fixture";
import { FixturePhoneClient } from "../src/fixture/phone";
import { Decide } from "../src/phone/Decide";
import { PhoneApp } from "../src/phone/PhoneApp";
import { trayInvoiceCard, TrayFixture } from "../src/fixture/tray";
import { TrayPanel } from "../src/tray/TrayPanel";

/** The invoice card as the engine draws it once someone said yes in Telegram. */
async function decidedInvoice(): Promise<Card> {
  const client = new FixtureClient();
  client.elsewhere.add("apr_invoice");
  const outcome = await client.command(
    { type: "decide", decisionId: "apr_invoice", version: 1, answer: "allowOnce", expanded: true },
    { kind: "pointer" },
  );
  return (outcome as Extract<CommandOutcome, { type: "alreadyDecided" }>).card;
}

function sendThrough(client: FixtureClient) {
  return vi.fn((command: Command, origin: Origin): Promise<CommandOutcome> => client.command(command, origin));
}

/** The one line of a decided card: the button that opens it. */
function lineOf(article: HTMLElement) {
  return within(article).getByRole("button", { expanded: false });
}

describe("a decided card in the home", () => {
  it("is one line: a check, the heading, then who answered and when", async () => {
    const card = await decidedInvoice();
    render(<DecisionCard card={card} send={vi.fn()} />);
    const article = screen.getByRole("article", { name: card.heading });
    const line = lineOf(article);
    expect(line.textContent).toBe("Send the October invoice to [CLIENT] · You said yes in Telegram at 07:31");
    expect(line.querySelector(".decided-check")).toBeTruthy();
    // The call, how it got here and the details wait behind the line.
    expect(within(article).queryByText("send_email")).toBeNull();
    expect(within(article).queryByText("Can't be unsent")).toBeNull();
    expect(within(article).queryAllByRole("button")).toEqual([line]);
  });

  it("collapses once answered here, and the line takes the focus", async () => {
    const client = new FixtureClient();
    render(<DecisionCard card={invoiceCard()} send={sendThrough(client)} />);
    await userEvent.click(screen.getByRole("button", { name: "Allow once" }));
    const article = screen.getByRole("article", { name: "Send the October invoice to [CLIENT]" });
    const line = await waitFor(() => lineOf(article));
    expect(within(line).getByText("You said yes in the home app at 07:40")).toBeTruthy();
    expect(document.activeElement).toBe(line);
    expect(within(article).queryByText("send_email")).toBeNull();
  });

  it("opens on a click, Enter or Space to show the exact call and the details, and closes again", async () => {
    const card = await decidedInvoice();
    render(<DecisionCard card={card} send={vi.fn()} />);
    const article = screen.getByRole("article", { name: card.heading });
    const line = lineOf(article);

    await userEvent.click(line);
    expect(line.getAttribute("aria-expanded")).toBe("true");
    const details = document.getElementById(line.getAttribute("aria-controls")!)!;
    expect(within(details).getByText("send_email")).toBeTruthy();
    expect(within(details).getByText("Can't be unsent")).toBeTruthy();
    expect(within(details).getByText("Medium risk")).toBeTruthy();
    expect(within(details).getByText("You said yes in Telegram at 07:31").getAttribute("aria-current")).toBe("step");
    await userEvent.click(line);
    expect(line.getAttribute("aria-expanded")).toBe("false");
    expect(within(article).queryByText("send_email")).toBeNull();

    line.focus();
    await userEvent.keyboard("{Enter}");
    expect(line.getAttribute("aria-expanded")).toBe("true");
    await userEvent.keyboard(" ");
    expect(line.getAttribute("aria-expanded")).toBe("false");
  });

  it("never answers on opening, Enter, Space or Esc, and offers no answer once open", async () => {
    const send = vi.fn();
    const card = await decidedInvoice();
    render(<DecisionCard card={card} send={send} />);
    const line = lineOf(screen.getByRole("article", { name: card.heading }));
    await userEvent.click(line);
    line.focus();
    await userEvent.keyboard("{Escape}{Enter}{Escape} ");
    expect(line.getAttribute("aria-expanded")).toBe("true");
    expect(send).not.toHaveBeenCalled();
    for (const label of ["Allow once", "Always allow for this person", "Edit", "Deny"]) {
      expect(screen.queryByRole("button", { name: label })).toBeNull();
    }
  });

  it("keeps its place in the thread as its line", async () => {
    const client = new FixtureClient();
    render(<App client={client} initialSection="threads" />);
    const thread = screen.getByRole("main");
    const card = await within(thread).findByRole("article", { name: "Send the October invoice to [CLIENT]" });
    const before = within(thread).getAllByRole("article").indexOf(card);
    await userEvent.click(within(card).getByRole("button", { name: "Allow once" }));
    await waitFor(() => expect(client.taken.commands).toHaveLength(1));
    // The open list no longer names it; the thread reads it back and keeps it where it was.
    await new Promise((resolve) => setTimeout(resolve, 100));
    const decided = await within(thread).findByRole("article", { name: "Send the October invoice to [CLIENT]" });
    expect(within(thread).getAllByRole("article").indexOf(decided)).toBe(before);
    expect(within(lineOf(decided)).getByText("You said yes in the home app at 07:40")).toBeTruthy();
  });
});

/** The browser's storage, in memory: the panel reads the theme from it. */
function memoryStorage(): Storage {
  const items = new Map<string, string>();
  return {
    get length() {
      return items.size;
    },
    clear: () => items.clear(),
    getItem: (key) => items.get(key) ?? null,
    key: (index) => [...items.keys()][index] ?? null,
    removeItem: (key) => void items.delete(key),
    setItem: (key, value) => void items.set(key, String(value)),
  };
}

/** A third and fourth everyday card on the tray, each yes once or no. */
function everyday(id: string, heading: string): Card {
  return { ...trayInvoiceCard(), id, heading, sessionId: `s-${id}` };
}

describe("an answered card on the tray", () => {
  beforeEach(() => {
    vi.stubGlobal("localStorage", memoryStorage());
  });

  afterEach(() => {
    vi.useRealTimers();
    delete document.documentElement.dataset.theme;
    vi.unstubAllGlobals();
  });

  it("collapses to one line, which opens and closes without answering", async () => {
    const client = new TrayFixture();
    render(<TrayPanel client={client} />);
    const invoice = await screen.findByRole("article", { name: "Send the October invoice" });
    await userEvent.click(within(invoice).getByRole("button", { name: "Allow once" }));
    const article = await screen.findByRole("article", { name: "Send the October invoice" });
    const line = await waitFor(() => lineOf(article));
    expect(within(line).getByText("You said yes from the tray at 07:40")).toBeTruthy();
    expect(document.activeElement).toBe(line);
    expect(within(article).queryByText(/^send_email\(/)).toBeNull();
    expect(within(article).queryByText("Can't be unsent")).toBeNull();

    await userEvent.click(line);
    expect(line.getAttribute("aria-expanded")).toBe("true");
    expect(within(article).getByText(/^send_email\(/)).toBeTruthy();
    expect(within(article).getByText("Can't be unsent")).toBeTruthy();
    line.focus();
    await userEvent.keyboard("{Escape}{Enter} {Escape}");
    expect(line.getAttribute("aria-expanded")).toBe("true");
    expect(within(article).queryAllByRole("button")).toEqual([line]);
    expect(client.taken.commands).toHaveLength(1);
  });

  it("drops off five minutes after the answer", async () => {
    vi.useFakeTimers({ shouldAdvanceTime: true, toFake: ["setTimeout", "clearTimeout", "setInterval", "clearInterval", "Date"] });
    const user = userEvent.setup({ advanceTimers: vi.advanceTimersByTime });
    render(<TrayPanel client={new TrayFixture()} />);
    const invoice = await screen.findByRole("article", { name: "Send the October invoice" });
    await user.click(within(invoice).getByRole("button", { name: "Allow once" }));
    await waitFor(() => lineOf(screen.getByRole("article", { name: "Send the October invoice" })));

    await act(async () => {
      vi.advanceTimersByTime(4 * 60_000 + 50_000);
    });
    expect(screen.getByRole("article", { name: "Send the October invoice" })).toBeTruthy();
    await act(async () => {
      vi.advanceTimersByTime(15_000);
    });
    expect(screen.queryByRole("article", { name: "Send the October invoice" })).toBeNull();
    expect(screen.getByRole("article", { name: "Publish a release" })).toBeTruthy();
  });

  it("drops off when a new decision arrives", async () => {
    const client = new TrayFixture();
    render(<TrayPanel client={client} />);
    const invoice = await screen.findByRole("article", { name: "Send the October invoice" });
    await userEvent.click(within(invoice).getByRole("button", { name: "Allow once" }));
    await waitFor(() => lineOf(screen.getByRole("article", { name: "Send the October invoice" })));

    // A read that brings nothing new keeps the line.
    act(() => {
      window.dispatchEvent(new Event("focus"));
    });
    await new Promise((resolve) => setTimeout(resolve, 50));
    expect(screen.getByRole("article", { name: "Send the October invoice" })).toBeTruthy();

    client.cards.set("apr_notes", everyday("apr_notes", "Back up the notes"));
    act(() => {
      window.dispatchEvent(new Event("focus"));
    });
    await screen.findByRole("article", { name: "Back up the notes" });
    await waitFor(() => expect(screen.queryByRole("article", { name: "Send the October invoice" })).toBeNull());
    expect(client.taken.commands).toHaveLength(1);
  });

  it("keeps the three newest lines", async () => {
    const client = new TrayFixture();
    const headings = ["Send the October invoice", "Back up the notes", "File the receipts", "Pay the rent"];
    for (const [index, heading] of headings.slice(1).entries()) {
      client.cards.set(`apr_${index}`, everyday(`apr_${index}`, heading));
    }
    render(<TrayPanel client={client} />);
    for (const heading of headings) {
      const article = await screen.findByRole("article", { name: heading });
      await userEvent.click(within(article).getByRole("button", { name: "Allow once" }));
      await waitFor(() => lineOf(screen.getByRole("article", { name: heading })));
    }
    expect(screen.queryByRole("article", { name: headings[0] })).toBeNull();
    for (const heading of headings.slice(1)) {
      expect(lineOf(screen.getByRole("article", { name: heading }))).toBeTruthy();
    }
  });
});

describe("a decided card on the phone", () => {
  it("is one line in Decisions, with how far the answer reaches, and opens in place", async () => {
    const client = new FixturePhoneClient();
    const answer = vi.spyOn(client, "answer");
    render(<PhoneApp client={client} initial={{ screen: "decisions" }} />);
    const article = await screen.findByRole("article", { name: "Send the October invoice" });
    const line = lineOf(article);
    expect(line.textContent).toBe("Send the October invoice · You said yes in Telegram at 07:31 · done once");
    expect(within(article).queryByText(/^send_email\(/)).toBeNull();

    await userEvent.click(line);
    expect(within(article).getByText(/^send_email\(/)).toBeTruthy();
    line.focus();
    await userEvent.keyboard("{Escape}{Enter} {Escape}");
    expect(line.getAttribute("aria-expanded")).toBe("true");
    expect(window.location.pathname).not.toContain("/decide");
    expect(answer).not.toHaveBeenCalled();
  });

  it("collapses its own screen once answered there, and the line takes the focus", async () => {
    const client = new FixturePhoneClient();
    render(<Decide client={client} id="apr_release" onBack={() => undefined} onSettled={() => undefined} />);
    await userEvent.click(await screen.findByRole("button", { name: /Confirm it's you and approve/ }));
    const article = await screen.findByRole("article", { name: "Publish a release" });
    const line = lineOf(article);
    expect(within(line).getByText("You said yes on your phone at 07:40")).toBeTruthy();
    expect(document.activeElement).toBe(line);
    expect(screen.queryByText("cargo publish -p [CRATE] in [REPO]", { normalizer: (text) => text.replace(/\s+/g, " ") })).toBeNull();
    await userEvent.click(line);
    expect(within(article).getByText(/cargo publish -p \[CRATE\]/)).toBeTruthy();
    expect(screen.queryByRole("button", { name: /Confirm it's you|Deny/ })).toBeNull();
  });
});

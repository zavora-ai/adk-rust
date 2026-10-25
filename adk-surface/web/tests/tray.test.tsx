// The tray panel (spec 3.3 §5.5, §7.1, §7.2 tray row, §7.4, S-I4, S-I10) over its example:
// yes once or no on everyday calls, the home for everything else, keys and hiding never
// decide, Stop all and never Resume all.
import { act, fireEvent, render, screen, waitFor, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import type { DecisionCard } from "../src/generated/api";
import { ApiError } from "../src/client";
import { trayInvoiceCard, TrayFixture } from "../src/fixture/tray";
import { shellOf } from "../src/route";
import { TrayPanel } from "../src/tray/TrayPanel";

/** The browser's storage, in memory: the panel shares the theme with the home through it. */
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

beforeEach(() => {
  vi.stubGlobal("localStorage", memoryStorage());
});

afterEach(() => {
  delete document.documentElement.dataset.theme;
  vi.unstubAllGlobals();
});

async function card(name: string) {
  return screen.findByRole("article", { name });
}

function buttons(article: HTMLElement) {
  return within(article)
    .queryAllByRole("button")
    .map((button) => button.textContent);
}

describe("the tray shell", () => {
  it("is what /ui/tray starts", () => {
    expect(shellOf({ pathname: "/ui/tray" })).toBe("tray");
    expect(shellOf({ pathname: "/ui/today" })).toBe("home");
  });
});

describe("a decision on the tray", () => {
  it("offers exactly Allow once and Deny on a medium card, with the exact call and its reversibility", async () => {
    render(<TrayPanel client={new TrayFixture()} />);
    const invoice = await card("Send the October invoice");
    expect(buttons(invoice)).toEqual(["Allow once", "Deny"]);
    expect(within(invoice).getByText(/^send_email\(to: /)).toBeTruthy();
    expect(within(invoice).getByText("Can't be unsent")).toBeTruthy();
    expect(within(invoice).getByText("Finance · since 18:00 · Medium risk")).toBeTruthy();
    // Ids and versions are never shown.
    expect(document.body.textContent).not.toMatch(/apr_invoice|version/);
  });

  it("never approves high-risk, money, truncated or presence-needing cards: Deny at most, and the home", async () => {
    const client = new TrayFixture();
    const base = trayInvoiceCard();
    const deny = base.allowed.options.find((option) => option.answer === "deny")!;
    const allow = base.allowed.options.find((option) => option.answer === "allowOnce")!;
    const variants: DecisionCard[] = [
      // What the engine gives the tray for money: Deny, and approval in the home.
      { ...base, id: "apr_money", heading: "Pay the supplier", money: true, allowed: { options: [deny], openInHome: true } },
      {
        ...base,
        id: "apr_long",
        heading: "Send the long letter",
        call: { ...base.call, truncated: true, text: "send_email(to: …" },
        allowed: { options: [{ ...allow, needsExpanded: true }, deny], openInHome: true },
      },
      {
        ...base,
        id: "apr_presence",
        heading: "Rotate the key",
        allowed: { options: [{ ...allow, needsPresence: true }, deny], openInHome: true },
      },
    ];
    for (const variant of variants) client.cards.set(variant.id, variant);
    render(<TrayPanel client={client} />);
    for (const [name, id] of [
      ["Publish a release", "apr_release"],
      ["Pay the supplier", "apr_money"],
      ["Send the long letter", "apr_long"],
      ["Rotate the key", "apr_presence"],
    ]) {
      const article = await card(name);
      expect(buttons(article)).toEqual(["Deny"]);
      expect(within(article).getByText(/· open to decide$/)).toBeTruthy();
      for (const link of within(article).getAllByRole("link")) {
        expect(link.getAttribute("href")).toBe(`/ui/decisions/${id}`);
        expect(link.getAttribute("target")).toBe("_blank");
      }
    }
    expect(within(await card("Publish a release")).getByText("Code · High risk · open to decide")).toBeTruthy();
    // Every card's heading opens the home at it, the everyday one too.
    const heading = within(await card("Send the October invoice")).getByRole("link", { name: "Send the October invoice" });
    expect(heading.getAttribute("href")).toBe("/ui/decisions/apr_invoice");
  });

  it("offers nothing on a card the engine gives the tray no answer for", async () => {
    const client = new TrayFixture();
    client.cards.set("apr_release", { ...client.cards.get("apr_release")!, allowed: { options: [], openInHome: true } });
    render(<TrayPanel client={client} />);
    const release = await card("Publish a release");
    expect(buttons(release)).toEqual([]);
    expect(within(release).getByText("Code · High risk · open to decide")).toBeTruthy();
  });

  it("allows once with the card's version, the call shown in full and the pointer", async () => {
    const client = new TrayFixture();
    render(<TrayPanel client={client} />);
    const invoice = await card("Send the October invoice");
    await userEvent.click(within(invoice).getByRole("button", { name: "Allow once" }));
    await within(invoice).findByText("You said yes from the tray at 07:40");
    expect(client.taken.commands).toEqual([
      {
        command: { type: "decide", decisionId: "apr_invoice", version: 1, answer: "allowOnce", expanded: true },
        origin: { kind: "pointer" },
      },
    ]);
    // What is left is the card's one line, which answers nothing.
    expect(buttons(invoice)).toEqual(["Send the October invoice · You said yes from the tray at 07:40"]);
  });

  it("never decides on Enter, Esc, hiding the panel or closing it", async () => {
    const client = new TrayFixture();
    const { unmount } = render(<TrayPanel client={client} />);
    const invoice = await card("Send the October invoice");
    const allow = within(invoice).getByRole("button", { name: "Allow once" });
    allow.focus();
    await userEvent.keyboard("{Enter}");
    await userEvent.keyboard("{Escape}");
    fireEvent.keyDown(within(invoice).getByRole("button", { name: "Deny" }), { key: "Enter" });
    act(() => {
      window.dispatchEvent(new Event("blur"));
      document.dispatchEvent(new Event("visibilitychange"));
    });
    unmount();
    await new Promise((resolve) => setTimeout(resolve, 50));
    expect(client.taken.commands).toEqual([]);
  });

  it("replaces a changed card and needs a new click", async () => {
    const client = new TrayFixture();
    const changed = { ...trayInvoiceCard(), version: 2, call: { ...trayInvoiceCard().call, text: 'send_email(to: "[other]")' } };
    client.nextDecide = { type: "changed", card: changed };
    render(<TrayPanel client={client} />);
    const invoice = await card("Send the October invoice");
    await userEvent.click(within(invoice).getByRole("button", { name: "Allow once" }));
    await within(invoice).findByText("This decision changed since you saw it. Check it again.");
    expect(within(invoice).getByText('send_email(to: "[other]")')).toBeTruthy();
    expect(client.taken.commands).toHaveLength(1);
    await userEvent.click(within(invoice).getByRole("button", { name: "Allow once" }));
    expect(client.taken.commands.map(({ command }) => command.type === "decide" && command.version)).toEqual([1, 2]);
  });

  it("says who answered first when the decision was already decided", async () => {
    const client = new TrayFixture();
    const decided: DecisionCard = {
      ...trayInvoiceCard(),
      version: 2,
      status: "decided",
      statusText: "Decided",
      allowed: { options: [], openInHome: false },
      decided: {
        answer: "yes",
        at: new Date().toISOString(),
        atText: "07:31",
        by: "You",
        byYou: true,
        text: "You said yes from the tray at 07:31",
        via: { surface: "tray", origin: { kind: "pointer" } },
      },
    };
    client.nextDecide = { type: "alreadyDecided", card: decided, decided: decided.decided! };
    render(<TrayPanel client={client} />);
    const invoice = await card("Send the October invoice");
    await userEvent.click(within(invoice).getByRole("button", { name: "Allow once" }));
    expect(await within(invoice).findByText("You said yes from the tray at 07:31")).toBeTruthy();
  });
});

describe("the panel", () => {
  it("shows the decisions badge the status line counts", async () => {
    render(<TrayPanel client={new TrayFixture()} />);
    const heading = await screen.findByRole("heading", { name: /^Decisions/ });
    await waitFor(() => expect(within(heading).getByLabelText("2 decisions wait").textContent).toBe("2"));
  });

  it("says all agents stopped after Stop all, and never offers Resume all", async () => {
    const client = new TrayFixture();
    render(<TrayPanel client={client} />);
    expect(await screen.findByText("Running")).toBeTruthy();
    await userEvent.click(screen.getByRole("button", { name: "Stop all" }));
    expect(await screen.findByText("All agents stopped")).toBeTruthy();
    expect(client.taken.commands.map(({ command }) => command.type)).toEqual(["stopAll"]);
    expect(screen.queryByRole("button", { name: "Resume all" })).toBeNull();
  });

  it("asks the primary agent, and says a message waits behind an open card", async () => {
    const client = new TrayFixture();
    client.primary = { name: "finance", displayName: "Finance", avatar: { monogram: "F", color: "#15803D" } };
    render(<TrayPanel client={client} />);
    const box = await screen.findByRole("textbox", { name: "Ask Finance" });
    await userEvent.type(box, "and the November one{Enter}");
    expect(await screen.findByText("Sent after this card is answered")).toBeTruthy();
    expect(client.taken.commands).toEqual([
      { command: { type: "send", sessionId: "s-finance", text: "and the November one" }, origin: { kind: "keyboard" } },
    ]);
  });

  it("asks your agents when the kernel names no primary agent", async () => {
    const client = new TrayFixture();
    client.primary = undefined;
    render(<TrayPanel client={client} />);
    expect(await screen.findByRole("textbox", { name: "Message your agents" })).toBeTruthy();
  });

  it("draws spend against the limit, no bar without a limit and no line without spend", async () => {
    const client = new TrayFixture();
    const { unmount } = render(<TrayPanel client={client} />);
    expect(await screen.findByText("$1.84 of $5.00 today")).toBeTruthy();
    const meter = screen.getByRole("meter");
    expect((meter.firstChild as HTMLElement).style.width).toBe("36.8%");
    unmount();

    client.line = { ...client.line, spend: { spentMicroUsd: 1_840_000, limitMicroUsd: null, text: "$1.84 today" } };
    const second = render(<TrayPanel client={client} />);
    expect(await screen.findByText("$1.84 today")).toBeTruthy();
    expect(screen.queryByRole("meter")).toBeNull();
    second.unmount();

    client.line = { ...client.line, spend: null };
    render(<TrayPanel client={client} />);
    await screen.findByText("Quiet from 22:00");
    expect(screen.queryByText(/today$/)).toBeNull();
  });

  it("clamps an overspent bar to full", async () => {
    const client = new TrayFixture();
    client.line = { ...client.line, spend: { spentMicroUsd: 7_000_000, limitMicroUsd: 5_000_000, text: "$7.00 of $5.00 today" } };
    render(<TrayPanel client={client} />);
    await screen.findByText("$7.00 of $5.00 today");
    expect((screen.getByRole("meter").firstChild as HTMLElement).style.width).toBe("100%");
  });

  it("follows the home's theme switch", async () => {
    render(<TrayPanel client={new TrayFixture()} />);
    await screen.findByText("Running");
    window.localStorage.setItem("adk.home.theme", "dark");
    act(() => {
      window.dispatchEvent(new StorageEvent("storage", { key: "adk.home.theme" }));
    });
    expect(document.documentElement.dataset.theme).toBe("dark");
  });

  it("reads again when it is shown", async () => {
    const client = new TrayFixture();
    let reads = 0;
    const status = client.status.bind(client);
    client.status = async () => {
      reads += 1;
      return status();
    };
    render(<TrayPanel client={client} />);
    await screen.findByText("Running");
    const before = reads;
    Object.defineProperty(document, "visibilityState", { configurable: true, value: "visible" });
    act(() => {
      document.dispatchEvent(new Event("visibilitychange"));
    });
    await waitFor(() => expect(reads).toBeGreaterThan(before));
  });

  it("asks for the token when the kernel needs one, and says when it cannot reach it", async () => {
    const client = new TrayFixture();
    client.failStatus = new ApiError(401, "the surface API needs an authenticated caller");
    const { unmount } = render(<TrayPanel client={client} />);
    expect((await screen.findByRole("alert")).textContent).toBe("Start the tray with --token");
    unmount();
    client.failStatus = new TypeError("Failed to fetch");
    render(<TrayPanel client={client} />);
    expect((await screen.findByRole("alert")).textContent).toBe("Can't reach your agents right now. Trying again.");
  });

  it("shows what is working now and what comes next", async () => {
    render(<TrayPanel client={new TrayFixture()} />);
    expect(await screen.findByText("Chief of staff · planning the week")).toBeTruthy();
    expect(screen.getByText("Working")).toBeTruthy();
    expect(await screen.findByText("Code · CI check")).toBeTruthy();
    expect(screen.getByText("09:00")).toBeTruthy();
  });
});

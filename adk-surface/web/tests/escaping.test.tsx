// S-I9 on the phone and the tray (spec 3.3 §11): every string from a model, a tool or a file is
// escaped before display. Markup arrives as text, never as an element, and bidi and zero-width
// characters as their escapes, as on the web home's cards and in the terminal.
import { render, screen, waitFor } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import type { DecisionCard } from "../src/generated/api";
import { FixturePhoneClient, phoneReleaseCard } from "../src/fixture/phone";
import { TrayFixture, trayInvoiceCard } from "../src/fixture/tray";
import { Decide } from "../src/phone/Decide";
import { PhoneApp } from "../src/phone/PhoneApp";
import { TrayPanel } from "../src/tray/TrayPanel";

const MARKUP = '<img src="x" onerror="window.__pwned = 1">';
const HIDDEN = ["​", "‮", "⁦", "⁩"];

/** `card` with a heading, summary, agent and call a hostile model or tool wrote. */
function hostile(card: DecisionCard): DecisionCard {
  return {
    ...card,
    heading: `Send invoice_‮fdp.exe ${MARKUP}`,
    summary: `${MARKUP} for ⁦Finance⁩`,
    agent: { ...card.agent, displayName: `Fin​ance ${MARKUP}` },
    call: {
      tool: "send_email",
      args: { to: "billing@corp​.com", note: MARKUP },
      text: `send_email(to: "billing@corp​.com", note: "${MARKUP}")`,
      truncated: false,
    },
  };
}

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

/** What the person reads holds the markup as text, no element of it, and no hidden character. */
function expectInert(container: HTMLElement) {
  const text = container.textContent ?? "";
  for (const hidden of HIDDEN) expect(text).not.toContain(hidden);
  expect(text).toContain("invoice_\\u{202e}fdp.exe");
  expect(text).toContain("<img");
  expect(container.querySelector("img[onerror]")).toBeNull();
  expect((window as { __pwned?: number }).__pwned).toBeUndefined();
}

beforeEach(() => {
  vi.stubGlobal("localStorage", memoryStorage());
});

afterEach(() => {
  vi.unstubAllGlobals();
});

describe("a hostile decision", () => {
  it("reads as inert text on the phone's list and its card", async () => {
    const client = new FixturePhoneClient();
    const card = hostile({ ...phoneReleaseCard(), status: "waiting" });
    client.cards = [card];
    const list = render(<PhoneApp client={client} initial={{ screen: "decisions" }} />);
    await waitFor(() => expect(list.container.textContent).toContain("\\u{202e}"));
    expectInert(list.container);
    list.unmount();

    const decide = render(<Decide client={client} id={card.id} onBack={() => undefined} onSettled={() => undefined} />);
    await waitFor(() => expect(decide.container.textContent).toContain("billing@corp\\u{200b}.com"));
    expectInert(decide.container);
    expect(decide.container.textContent).toContain("\\u{2066}Finance\\u{2069}");
  });

  it("reads as inert text on the tray", async () => {
    const client = new TrayFixture();
    const card = hostile(trayInvoiceCard());
    client.cards = new Map([[card.id, card]]);
    const { container } = render(<TrayPanel client={client} />);
    await screen.findAllByRole("article");
    await waitFor(() => expect(container.textContent).toContain("billing@corp\\u{200b}.com"));
    expectInert(container);
    expect(container.textContent).toContain("Fin\\u{200b}ance");
  });
});

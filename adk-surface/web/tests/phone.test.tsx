// The phone (spec 3.3 §5.4, §7.4, §8.2): the badge on the heading and the tab, the high-risk
// card's words, "Confirm it's you" through options, the passkey and the bound answer — never the
// plain answer route — Enter and Esc never decide, a card answered elsewhere replaces the
// waiting one, a changed card sends no second answer, the device token across a reload, and
// iOS's "Add to Home Screen first".
import { render, screen, waitFor, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, describe, expect, it, vi } from "vitest";
import { ApiError } from "../src/client";
import type { CommandOutcome, DecisionCard } from "../src/generated/api";
import { FixturePhoneClient, phoneDecidedInvoice, phoneReleaseCard } from "../src/fixture/phone";
import { HttpPhoneClient } from "../src/phone/client";
import type { PhoneFrame } from "../src/phone/client";
import { Decide } from "../src/phone/Decide";
import { memoryTokens } from "../src/phone/idb";
import { Pair } from "../src/phone/Pair";
import { PhoneApp } from "../src/phone/PhoneApp";

afterEach(() => {
  vi.restoreAllMocks();
  vi.unstubAllGlobals();
});

function json(body: unknown, status = 200) {
  return new Response(JSON.stringify(body), { status, headers: { "content-type": "application/json" } });
}

describe("the phone", () => {
  it("shows the waiting count on the heading and the tab", async () => {
    render(<PhoneApp client={new FixturePhoneClient()} initial={{ screen: "decisions" }} />);
    const heading = await screen.findByRole("heading", { name: /Decisions/ });
    await waitFor(() => expect(within(heading).getByText("1")).toBeTruthy());
    const tab = screen.getByRole("button", { name: /Decisions/ });
    expect(within(tab).getByText("1")).toBeTruthy();
  });

  it("says what a high-risk card needs, and how a decided one was answered", async () => {
    render(<PhoneApp client={new FixturePhoneClient()} initial={{ screen: "decisions" }} />);
    expect(await screen.findByText("Can't be undone · needs your face or fingerprint")).toBeTruthy();
    expect(screen.getByText("You said yes in Telegram at 07:31 · done once")).toBeTruthy();
    expect(screen.queryByRole("textbox")).toBeNull();
    expect(screen.queryByText("Resume all")).toBeNull();
  });

  it("confirms it's you through options, the passkey and the bound answer, never the answer route", async () => {
    const card = phoneReleaseCard();
    const answered: DecisionCard = { ...phoneDecidedInvoice(), id: card.id, heading: card.heading };
    const calls: string[] = [];
    vi.stubGlobal(
      "fetch",
      vi.fn(async (url: string, init?: RequestInit) => {
        calls.push(`${init?.method ?? "GET"} ${url}`);
        if (url.endsWith("/confirm/options")) {
          return json({ challengeId: "c1", publicKey: { challenge: "AAAA", rpId: "localhost", allowCredentials: [{ type: "public-key", id: "AQID" }] } });
        }
        if (url.endsWith("/confirm")) return json({ type: "answered", card: answered } satisfies CommandOutcome);
        return json(card);
      }),
    );
    const get = vi.fn(async () => ({
      toJSON: () => ({ id: "AQID", rawId: "AQID", type: "public-key", response: {} }),
    }));
    vi.stubGlobal("navigator", { ...navigator, credentials: { get } });
    const client = await HttpPhoneClient.open(memoryTokens("adk_dev_token"));
    render(<Decide client={client} id={card.id} onBack={() => undefined} onSettled={() => undefined} />);
    await userEvent.click(await screen.findByRole("button", { name: /Confirm it's you and approve/ }));
    await screen.findByText(/You said yes/);
    expect(get).toHaveBeenCalledTimes(1);
    const writes = calls.filter((call) => call.startsWith("POST"));
    expect(writes).toEqual([
      `POST /api/surface/decisions/${card.id}/confirm/options`,
      `POST /api/surface/decisions/${card.id}/confirm`,
    ]);
    expect(calls.some((call) => call.includes("/answer"))).toBe(false);
  });

  it("offers Deny only and the home when the passkey check fails, and never approves without presence", async () => {
    const client = new FixturePhoneClient();
    const confirm = vi
      .spyOn(client, "confirm")
      .mockRejectedValue(new ApiError(403, "Your face or fingerprint was not checked.", "surface.phone.passkey_not_used"));
    const answer = vi.spyOn(client, "answer");
    render(<Decide client={client} id="apr_release" onBack={() => undefined} onSettled={() => undefined} />);
    await userEvent.click(await screen.findByRole("button", { name: /Confirm it's you and approve/ }));
    expect(await screen.findByText("Approve it in the home on your computer")).toBeTruthy();
    expect(screen.queryByRole("button", { name: /Confirm it's you and approve/ })).toBeNull();
    expect(screen.getAllByRole("button").map((button) => button.textContent?.trim())).toContain("Deny");
    expect(confirm).toHaveBeenCalledTimes(1);
    expect(answer).not.toHaveBeenCalled();
  });

  it("offers Deny only and the home on a phone without its passkey", async () => {
    const client = new FixturePhoneClient();
    vi.spyOn(client, "info").mockResolvedValue({ pushKey: "BExample", rpId: "localhost", passkey: false, push: true, canAddPasskey: false });
    render(<Decide client={client} id="apr_release" onBack={() => undefined} onSettled={() => undefined} />);
    expect(await screen.findByText("Approve it in the home on your computer")).toBeTruthy();
    expect(screen.queryByRole("button", { name: /Confirm it's you and approve/ })).toBeNull();
  });

  it("never decides on Enter or Esc", async () => {
    const client = new FixturePhoneClient();
    const answer = vi.spyOn(client, "answer");
    const confirm = vi.spyOn(client, "confirm");
    render(<Decide client={client} id="apr_release" onBack={() => undefined} onSettled={() => undefined} />);
    const button = await screen.findByRole("button", { name: /Confirm it's you and approve/ });
    button.focus();
    await userEvent.keyboard("{Enter}");
    await userEvent.keyboard("{Escape}");
    screen.getByRole("button", { name: "Deny" }).focus();
    await userEvent.keyboard("{Enter}");
    expect(answer).not.toHaveBeenCalled();
    expect(confirm).not.toHaveBeenCalled();
  });

  it("shows a card answered elsewhere from the feed in place of the waiting one", async () => {
    const client = new FixturePhoneClient();
    let release: (frame: PhoneFrame) => void = () => undefined;
    vi.spyOn(client, "watch").mockImplementation(async function* () {
      yield await new Promise<PhoneFrame>((resolve) => {
        release = resolve;
      });
    });
    render(<Decide client={client} id="apr_release" onBack={() => undefined} onSettled={() => undefined} />);
    await screen.findByRole("button", { name: /Confirm it's you and approve/ });
    client.cards = client.cards.map((card) =>
      card.id === "apr_release"
        ? { ...phoneDecidedInvoice(), id: card.id, heading: card.heading, decided: { ...phoneDecidedInvoice().decided!, text: "You said yes in Telegram at 07:45" } }
        : card,
    );
    release({ type: "decisions", cards: [] });
    expect(await screen.findByText(/You said yes in Telegram at 07:45/)).toBeTruthy();
    expect(screen.queryByRole("button", { name: /Confirm it's you and approve/ })).toBeNull();
  });

  it("sends no second answer when the decision changed", async () => {
    const client = new FixturePhoneClient();
    const changed = { ...phoneReleaseCard(), version: 2 };
    const confirm = vi.spyOn(client, "confirm").mockResolvedValue({ type: "changed", card: changed });
    render(<Decide client={client} id="apr_release" onBack={() => undefined} onSettled={() => undefined} />);
    await userEvent.click(await screen.findByRole("button", { name: /Confirm it's you and approve/ }));
    expect(await screen.findByText(/changed since you saw it/)).toBeTruthy();
    expect(confirm).toHaveBeenCalledTimes(1);
    await userEvent.click(screen.getByRole("button", { name: /Confirm it's you and approve/ }));
    expect(confirm).toHaveBeenLastCalledWith(expect.objectContaining({ version: 2 }), "allowOnce", true, undefined);
  });

  it("keeps its device token across a reload", async () => {
    vi.stubGlobal("fetch", vi.fn(async () => json({ device: {}, token: "adk_dev_kept" })));
    const tokens = memoryTokens();
    const first = await HttpPhoneClient.open(tokens);
    expect(first.paired()).toBe(false);
    await first.claim("K7Q4-9MXC");
    const reloaded = await HttpPhoneClient.open(tokens);
    expect(reloaded.paired()).toBe(true);
    await reloaded.signOut();
    expect((await HttpPhoneClient.open(tokens)).paired()).toBe(false);
  });

  it("asks to add the app to the Home Screen first in Safari on an iPhone", () => {
    vi.stubGlobal("navigator", { ...navigator, userAgent: "Mozilla/5.0 (iPhone; CPU iPhone OS 18_0 like Mac OS X) Safari/604.1" });
    render(<Pair client={new FixturePhoneClient(false)} code="K7Q4-9MXC" onDone={() => undefined} />);
    expect(screen.getByRole("heading", { name: "Add to Home Screen first" })).toBeTruthy();
    expect(screen.getByText("K7Q4-9MXC")).toBeTruthy();
    expect(screen.queryByRole("textbox")).toBeNull();
  });

  it("goes on to the passkey when notifications cannot be turned on, and says why", async () => {
    const client = new FixturePhoneClient(false);
    vi.spyOn(client, "allowNotifications").mockRejectedValue(new ApiError(400, "this push endpoint is refused"));
    render(<Pair client={client} code="K7Q4-9MXC" onDone={() => undefined} />);
    await userEvent.click(screen.getByRole("button", { name: "Pair this phone" }));
    await userEvent.click(await screen.findByRole("button", { name: "Allow notifications" }));
    expect(await screen.findByText("Notifications are off: this push endpoint is refused")).toBeTruthy();
    expect(screen.queryByRole("button", { name: "Allow notifications" })).toBeNull();
    expect(screen.queryByRole("alert")).toBeNull();
  });
});

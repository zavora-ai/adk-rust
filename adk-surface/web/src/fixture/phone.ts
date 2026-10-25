// The phone's screens over the example of spec 3.3 §5.4, for `npm run dev` at
// /ui/fixture.html?screen=phone and the screenshots beside the spec's images.
import type { AgentRow, Answer, CommandOutcome, DecisionCard, PhoneInfo, StatusLine } from "../generated/api";
import type { PhoneClient, PhoneFrame } from "../phone/client";
import { agents, briefing, invoiceCard, releaseCard, status } from "./fixture";

/** Code's release, as the concept shows it: high risk, the exact command in full. */
export function phoneReleaseCard(): DecisionCard {
  return {
    ...releaseCard(),
    sinceText: "since 07:32",
    call: {
      tool: "bash",
      args: { command: "cargo publish -p [CRATE]", cwd: "[REPO]" },
      text: "cargo publish -p [CRATE]\nin [REPO]",
      truncated: false,
    },
    timeline: [
      { text: "Asked at 07:32", current: false },
      { text: "Waiting for you", current: true },
    ],
  };
}

/** Finance's invoice, answered in Telegram at 07:31. */
export function phoneDecidedInvoice(): DecisionCard {
  const card = invoiceCard();
  return {
    ...card,
    heading: "Send the October invoice",
    status: "decided",
    statusText: "Decided",
    allowed: { ...card.allowed, options: [] },
    decided: {
      answer: "yes",
      at: card.waitingSince,
      atText: "07:31",
      by: "You",
      byYou: true,
      text: "You said yes in Telegram at 07:31",
      via: { surface: "chat", origin: { kind: "chat", channel: "Telegram" } },
      scopeText: "done once",
    },
  };
}

/** A phone client over the example, answering in memory. */
export class FixturePhoneClient implements PhoneClient {
  cards: DecisionCard[] = [phoneReleaseCard(), phoneDecidedInvoice()];
  constructor(private isPaired = true) {}

  paired() {
    return this.isPaired;
  }
  async status(): Promise<StatusLine> {
    return { ...status(), decisions: this.cards.filter((card) => card.status === "waiting").length };
  }
  async agents(): Promise<AgentRow[]> {
    return Object.values(agents).map((agent) => ({ agent, status: "waitingForYou", statusText: "Waiting for you" }));
  }
  async briefing() {
    return briefing();
  }
  async decisions() {
    return this.cards;
  }
  async decision(id: string) {
    const card = this.cards.find((known) => known.id === id);
    if (!card) throw new Error(`no decision ${id}`);
    return card;
  }
  private settle(card: DecisionCard, answer: Answer): CommandOutcome {
    const decided: DecisionCard = {
      ...card,
      status: "decided",
      statusText: "Decided",
      allowed: { ...card.allowed, options: [] },
      decided: {
        answer: answer === "deny" ? "no" : "yes",
        at: new Date().toISOString(),
        atText: "07:40",
        by: "You",
        byYou: true,
        text: answer === "deny" ? "You said no on your phone at 07:40" : "You said yes on your phone at 07:40",
      },
    };
    this.cards = this.cards.map((known) => (known.id === card.id ? decided : known));
    return { type: "answered", card: decided };
  }
  async answer(card: DecisionCard, answer: Answer) {
    return this.settle(card, answer);
  }
  async takeOver(card: DecisionCard) {
    return this.settle(card, "allowOnce");
  }
  async confirm(card: DecisionCard, answer: Answer) {
    return this.settle(card, answer);
  }
  async stop(): Promise<CommandOutcome> {
    return { type: "stopped", text: "All agents stopped" };
  }
  async info(): Promise<PhoneInfo> {
    return { pushKey: "BExample", rpId: "home.example.ts.net", passkey: true, push: true, canAddPasskey: false };
  }
  async claim() {
    this.isPaired = true;
  }
  async allowNotifications() {
    return true;
  }
  async addPasskey() {}
  async signOut() {
    this.isPaired = false;
  }
  async *watch(): AsyncIterable<PhoneFrame> {}
}

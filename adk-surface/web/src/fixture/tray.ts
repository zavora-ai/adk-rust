/**
 * The tray panel over the example of spec 3.3 §5.5 (`tray-light.png`), held in memory, for
 * component tests and the screenshots beside the spec's images. It answers as the engine
 * does for the tray row (§7.2): Allow once and Deny on everyday calls, nothing on high risk.
 * Never part of the built page.
 */
import type {
  AgentsResponse,
  Command,
  CommandOutcome,
  DecisionCard,
  Origin,
  StatusLine,
} from "../generated/api";
import { ApiError } from "../client";
import type { AgendaItem, KernelProcess } from "../kernel";
import type { TrayClient } from "../tray/TrayPanel";
import { agents, invoiceCard, processes, releaseCard, status } from "./fixture";

/** The Finance card as the tray sees it: yes once or no, the rest in the home. */
export function trayInvoiceCard(): DecisionCard {
  const card = invoiceCard();
  return {
    ...card,
    heading: "Send the October invoice",
    allowed: {
      options: card.allowed.options.filter((option) => option.answer === "allowOnce" || option.answer === "deny"),
      openInHome: true,
    },
  };
}

/** The Code card as the tray sees it: high risk, approved only in the home; saying no is safe anywhere. */
export function trayReleaseCard(): DecisionCard {
  return {
    ...releaseCard(),
    allowed: {
      options: [{ answer: "deny", label: "Deny", needsPresence: false, needsExpanded: false }],
      openInHome: true,
      note: "Approve it in the home app, or in a terminal on the computer your agents run on.",
    },
  };
}

/** Every call the fixture took, for tests. */
export interface TrayTaken {
  commands: Array<{ command: Command; origin: Origin }>;
}

export class TrayFixture implements TrayClient {
  readonly taken: TrayTaken = { commands: [] };
  cards = new Map<string, DecisionCard>([
    ["apr_invoice", trayInvoiceCard()],
    ["apr_release", trayReleaseCard()],
  ]);
  line: StatusLine = status();
  primary: AgentsResponse["primary"] = agents.chief;
  /** The next decide answers this instead of recording the answer. */
  nextDecide?: CommandOutcome;
  /** The status read fails with this, for tests. */
  failStatus?: unknown;

  async status() {
    if (this.failStatus) throw this.failStatus;
    return { ...this.line, decisions: (await this.decisions()).length };
  }

  async decisions() {
    return [...this.cards.values()].filter((card) => card.status === "waiting");
  }

  async agentsWithPrimary(): Promise<AgentsResponse> {
    return {
      agents: [
        { agent: agents.chief, status: "working", statusText: "Working", sessionId: "s-chief" },
        { agent: agents.finance, status: "waitingForYou", statusText: "Waiting for you", sessionId: "s-finance" },
        { agent: agents.code, status: "waitingForYou", statusText: "Waiting for you", sessionId: "s-code" },
      ],
      primary: this.primary,
    };
  }

  async running(): Promise<KernelProcess[]> {
    return [
      {
        run: { runId: "r-chief", target: "chief", state: "running", sessionId: "s-chief", input: { text: "planning the week" } },
        meta: { pid: "r-chief", labels: { title: "planning the week" } },
      },
    ];
  }

  async processes(): Promise<KernelProcess[]> {
    return processes.slice(0, 1);
  }

  async agenda(): Promise<AgendaItem[]> {
    return [];
  }

  async command(command: Command, origin: Origin): Promise<CommandOutcome> {
    this.taken.commands.push({ command, origin });
    switch (command.type) {
      case "decide": {
        const card = this.cards.get(command.decisionId);
        if (!card) throw new ApiError(404, "decision not found", "surface.not_found");
        if (this.nextDecide) {
          const outcome = this.nextDecide;
          this.nextDecide = undefined;
          if (outcome.type !== "pending" && "card" in outcome) this.cards.set(card.id, outcome.card);
          return outcome;
        }
        const said = command.answer === "deny" ? "You said no from the tray at 07:40" : "You said yes from the tray at 07:40";
        const decided: DecisionCard = {
          ...card,
          version: card.version + 1,
          status: "decided",
          statusText: "Decided",
          allowed: { options: [], openInHome: false },
          decided: {
            answer: command.answer === "deny" ? "no" : "yes",
            at: new Date().toISOString(),
            atText: "07:40",
            by: "You",
            byYou: true,
            text: said,
            via: { surface: "tray", origin },
          },
        };
        this.cards.set(card.id, decided);
        return { type: "answered", card: decided };
      }
      case "send": {
        const open = [...this.cards.values()].some((card) => card.sessionId === command.sessionId && card.status === "waiting");
        return open
          ? { type: "queued", sessionId: command.sessionId, position: 1, text: "Sent after this card is answered" }
          : { type: "sent", sessionId: command.sessionId };
      }
      case "stopAll":
        this.line = status(true);
        return { type: "stopped", text: "All agents stopped" };
      default:
        throw new ApiError(403, "the tray does not do that", "surface.not_here");
    }
  }
}

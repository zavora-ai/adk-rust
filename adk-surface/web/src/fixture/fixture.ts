/**
 * A client over the example of spec 3.3 §5.1 (Finance, Code, Research …), held in memory,
 * for component tests and for rendering the screens beside the spec's images. It answers as
 * the engine does: a decision answered once is decided, a second answer to the same version
 * is "already decided". Never part of the built page.
 */
import type {
  AgentRef,
  AgentRow,
  Briefing,
  Command,
  CommandOutcome,
  DecisionCard,
  FeedEntry,
  Origin,
  StatusLine,
} from "../generated/api";
import type { FeedMessage, HomeClient } from "../client";
import { ApiError } from "../client";
import type { AgendaItem, AreaSettings, AwayLine, AwayPage, KernelProcess, StandingRule, WakeSet } from "../kernel";
import type { Seams } from "../seams";

const ref = (name: string, displayName: string, monogram: string, color: string): AgentRef => ({
  name,
  displayName,
  avatar: { monogram, color },
});

export const agents = {
  chief: ref("chief", "Chief of staff", "C", "#C03A00"),
  finance: ref("finance", "Finance", "F", "#15803D"),
  code: ref("code", "Code", "{ }", "#1D4ED8"),
  research: ref("research", "Research", "R", "#7C3AED"),
  household: ref("household", "Household", "H", "#475569"),
};

function at(hh: number, mm: number, dayOffset = 0): string {
  const date = new Date();
  date.setDate(date.getDate() + dayOffset);
  date.setHours(hh, mm, 0, 0);
  return date.toISOString();
}

export function invoiceCard(): DecisionCard {
  return {
    id: "apr_invoice",
    version: 1,
    sessionId: "s-finance",
    agent: agents.finance,
    waitingSince: at(18, 0, -1),
    sinceText: "since 18:00",
    status: "waiting",
    statusText: "Waiting for you",
    heading: "Send the October invoice to [CLIENT]",
    call: {
      tool: "send_email",
      args: { to: "[client email]", subject: "Invoice INV-[NUMBER]", attachment: "invoice.pdf" },
      text: 'send_email(to: "[client email]", subject: "Invoice INV-[NUMBER]", attachment: "invoice.pdf")',
      truncated: false,
    },
    timeline: [
      { text: "Asked at 18:00", current: false },
      { text: "Waited through quiet time", current: false },
      { text: "Waiting for you", current: true },
    ],
    allowed: {
      options: [
        { answer: "allowOnce", label: "Allow once", needsPresence: false, needsExpanded: false },
        { answer: "alwaysAllow", label: "Always allow for this person", needsPresence: false, needsExpanded: false },
        { answer: "edit", label: "Edit", needsPresence: false, needsExpanded: false },
        { answer: "deny", label: "Deny", needsPresence: false, needsExpanded: false },
      ],
      openInHome: false,
    },
    risk: "medium",
    riskText: "Medium risk",
    reversibility: "cantBeUnsent",
    reversibilityText: "Can't be unsent",
    money: false,
    alsoShown: "Also on your phone and in Telegram",
  };
}

export function releaseCard(): DecisionCard {
  return {
    ...invoiceCard(),
    id: "apr_release",
    sessionId: "s-code",
    agent: agents.code,
    sinceText: "since 06:40",
    waitingSince: at(6, 40),
    heading: "Publish a release",
    call: { tool: "publish_release", args: { tag: "v3.0.0" }, text: 'publish_release(tag: "v3.0.0")', truncated: false },
    timeline: [
      { text: "Asked at 06:40", current: false },
      { text: "Waiting for you", current: true },
    ],
    allowed: {
      options: [
        { answer: "allowOnce", label: "Confirm it's you and approve", needsPresence: true, needsExpanded: false },
        { answer: "edit", label: "Edit", needsPresence: true, needsExpanded: false },
        { answer: "deny", label: "Deny", needsPresence: false, needsExpanded: false },
      ],
      openInHome: false,
    },
    risk: "high",
    riskText: "High risk",
    reversibility: "cantBeUndone",
    reversibilityText: "Can't be undone",
    alsoShown: "Also on your phone",
  };
}

/** A take over (spec 3.3 §8.2, §10 H2): Household's bank sign-in asks for a one-time code. */
export function takeOverCard(): DecisionCard {
  return {
    ...invoiceCard(),
    id: "apr_bank_code",
    sessionId: "s-household",
    agent: agents.household,
    sinceText: "since 07:12",
    waitingSince: at(7, 12),
    heading: "Type the code your bank sent",
    call: {
      tool: "harness:take-over",
      args: { takeOver: true, tool: "bank_sign_in", secret: "oneTimeCode", resource: "https://bank.example.com/login" },
      text: "harness:take-over(…)",
      truncated: false,
    },
    timeline: [
      { text: "Asked at 07:12", current: false },
      { text: "Waiting for you", current: true },
    ],
    allowed: { options: [{ answer: "deny", label: "Deny", needsPresence: false, needsExpanded: false }], openInHome: false },
    risk: "medium",
    riskText: "Medium risk",
    reversibility: "cantBeUndone",
    reversibilityText: "Can't be undone",
    alsoShown: "Also on your phone",
  };
}

function financeThread(): FeedEntry[] {
  return [
    {
      seq: 1,
      at: at(17, 58, -1),
      atText: "17:58",
      items: [
        {
          kind: "message",
          from: "agent",
          author: "Finance",
          text: "The October invoice for [CLIENT] is ready. I drafted it from the timesheet and your rate card, and I'd like to email it.",
        },
      ],
    },
    {
      seq: 2,
      at: at(17, 59, -1),
      atText: "17:59",
      items: [{ kind: "activity", tool: "read_file", text: "Read file timesheet-october.csv", callId: "c1" }],
    },
    { seq: 3, at: at(17, 59, -1), atText: "17:59", items: [{ kind: "result", tool: "read_file", ok: true, text: "Done", callId: "c1" }] },
    {
      seq: 4,
      at: at(18, 0, -1),
      atText: "18:00",
      items: [{ kind: "decision", tool: "send_email", text: "Waiting for you", decisionId: "apr_invoice" }],
    },
  ];
}

export const processes: KernelProcess[] = [
  ["p1", "code", "CI check", "0 9 * * *"],
  ["p2", "research", "News digest", "0 12 * * *"],
  ["p3", "finance", "Reconcile", "30 17 * * *"],
].map(([pid, target, title, cron]) => {
  const [minute, hour] = cron!.split(" ").map(Number);
  const local = new Date();
  local.setHours(hour!, minute!, 0, 0);
  return {
    run: { runId: pid!, target: target!, state: "waiting", input: { text: title } },
    meta: {
      pid: pid!,
      wake: { anyOf: [{ cron: { expr: `${local.getUTCMinutes()} ${local.getUTCHours()} * * *`, missed: "skip" } }] },
      labels: { title: title! },
    },
  };
});

export const agenda: AgendaItem[] = [
  {
    id: "ag_supplier",
    title: "Finance suggests paying the [SUPPLIER] invoice due Monday.",
    origin: { agent: { pid: "p3" } },
    state: "proposed",
    createdAt: at(23, 5, -1),
  },
  {
    id: "ag_fridays",
    title: "You seem to start work at 08:30 on Fridays now.",
    origin: { observation: { id: "obs-1" } },
    state: "proposed",
    createdAt: at(6, 0),
  },
];

export const rules: StandingRule[] = [
  {
    id: "rule_accountant",
    tool: "send_email",
    label: "Finance can email your accountant.",
    decision: { kind: "allow" },
    createdAt: at(9, 0, -20),
    expiresAt: new Date(new Date().getFullYear(), 10, 30, 12).toISOString(),
  },
  { id: "rule_tests", tool: "bash", label: "Code can run the tests in [REPO].", decision: { kind: "allow" }, createdAt: at(9, 0, -10) },
];

export function briefing(): Briefing {
  return {
    title: "While you were away",
    since: at(22, 4, -1),
    sinceText: "Since 22:04",
    spend: { spentMicroUsd: 620_000, limitMicroUsd: 5_000_000, text: "$0.62 of $5.00 today" },
    waiting: 3,
    lines: [
      { seq: 11, at: at(22, 40, -1), atText: "22:40", agent: "Research", pill: "done", pillText: "Done", text: 'Research finished "Compare health insurers", with three sources.' },
      { seq: 12, at: at(23, 30, -1), atText: "23:30", agent: "Code", pill: "youDecided", pillText: "You decided", text: "From your phone, you allowed Code to run the tests once. They ran once and passed." },
      { seq: 13, at: at(2, 0), atText: "02:00", agent: "Household", pill: "failed", pillText: "Failed", text: "The backup to [DRIVE] failed because the disk is full. Household will retry when you say so." },
    ],
  };
}

/** The example's "While you were away" as the kernel reads it (spec 3.3 §10 K4). */
export function awayPage(): AwayPage {
  const line = (seq: number, at: string, agent: string, kind: string, effect?: string): AwayLine => ({
    seq,
    at,
    partition: "default",
    agent,
    kind,
    ...(effect ? { effect } : {}),
    record: `/api/kernel/away/lines/${seq}`,
  });
  return {
    viewer: "home",
    since: at(22, 4, -1),
    through: 30,
    more: false,
    counts: { done: 1, failed: 1, stopped: 0, suggestions: 1, questions: 1, decided: 1 },
    spentMicroUsd: 620_000,
    quietTime: { respected: true },
    waiting: 3,
    lines: [
      line(11, at(22, 40, -1), "urn:adk:agent:home/research", "done", "completed"),
      line(14, at(23, 5, -1), "kernel", "suggestion", "proposed"),
      line(17, at(23, 30, -1), "urn:adk:agent:home/code", "decided", "approve"),
      line(22, at(2, 0), "urn:adk:agent:home/household", "failed", "failed"),
      line(29, at(6, 0), "kernel", "question"),
    ],
  };
}

/** The example's one area (spec 3.3 §10 K3). */
export function area(): AreaSettings {
  return {
    partition: "default",
    quietTime: [{ start: "22:00", end: "07:00" }],
    dailySpendingLimit: 5_000_000,
    spentToday: 1_840_000,
    mode: "ask",
  };
}

/** The status line of the example. */
export function status(stopped = false): StatusLine {
  return {
    mode: { mode: "askFirst", label: "Ask first", status: "Asks first" },
    spend: { spentMicroUsd: 1_840_000, limitMicroUsd: 5_000_000, text: "$1.84 of $5.00 today" },
    quiet: { start: "22:00", end: "07:00", active: false, text: "Quiet from 22:00" },
    decisions: 2,
    stopped,
    stoppedText: stopped ? "All agents stopped" : null,
  };
}

/** Every call the fixture took, for tests. */
export interface Taken {
  commands: Array<{ command: Command; origin: Origin }>;
}

export class FixtureClient implements HomeClient {
  readonly taken: Taken = { commands: [] };
  private cards = new Map<string, DecisionCard>([
    ["apr_invoice", invoiceCard()],
    ["apr_release", releaseCard()],
  ]);
  private stopped = false;
  private ruleList = [...rules];
  private agendaList = [...agenda];
  /** Answers already given elsewhere: the next decide of that card is "already decided". */
  readonly elsewhere = new Set<string>();
  /** What waits behind each session's open decisions, as the engine keeps it. */
  private queues = new Map<string, string[]>();
  /** The session a roster row names, by agent, where a test moves it (`null`: none). */
  readonly sessionOf: Record<string, string | null> = {};

  /** Puts `card` in the inbox, for tests. */
  hold(card: DecisionCard) {
    this.cards.set(card.id, card);
  }

  rows(): AgentRow[] {
    const waiting = (id: string) => this.cards.get(id)?.status === "waiting";
    const rows: AgentRow[] = [
      { agent: agents.chief, status: "working", statusText: "Working", sessionId: "s-chief" },
      waiting("apr_invoice")
        ? { agent: agents.finance, status: "waitingForYou", statusText: "Waiting for you", sessionId: "s-finance" }
        : { agent: agents.finance, status: "working", statusText: "Working", sessionId: "s-finance" },
      { agent: agents.code, status: "waitingForYou", statusText: "Waiting for you", sessionId: "s-code" },
      { agent: agents.research, status: "done", statusText: "Done", sessionId: "s-research" },
      { agent: agents.household, status: "asleep", statusText: "Asleep until 07:00", sessionId: "s-household", wakeAt: at(7, 0, 1) },
    ];
    return rows.map((row) => (row.agent.name in this.sessionOf ? { ...row, sessionId: this.sessionOf[row.agent.name] } : row));
  }

  async status() {
    return { ...status(this.stopped), decisions: (await this.decisions()).length };
  }

  async agents() {
    return this.rows();
  }

  async decisions() {
    return [...this.cards.values()].filter((card) => card.status === "waiting");
  }

  async decision(id: string) {
    const card = this.cards.get(id);
    if (!card) throw new ApiError(404, `decision '${id}' not found`, "surface.not_found");
    return card;
  }

  async briefing() {
    return briefing();
  }

  async command(command: Command, origin: Origin): Promise<CommandOutcome> {
    this.taken.commands.push({ command, origin });
    switch (command.type) {
      case "decide": {
        const card = this.cards.get(command.decisionId);
        if (!card) throw new ApiError(404, "decision not found", "surface.not_found");
        const option = card.allowed.options.find((candidate) => candidate.answer === command.answer);
        if (option?.needsPresence) {
          throw new ApiError(
            403,
            "confirm it's you first: high-risk calls are approved only after a passkey or device check",
            "surface.confirm_presence",
          );
        }
        if (card.status === "decided" || this.elsewhere.has(card.id)) {
          const decided = decidedCard(card, "You said yes in Telegram at 07:31", false);
          this.cards.set(card.id, decided);
          return { type: "alreadyDecided", card: decided, decided: decided.decided! };
        }
        // Still open, but changed since the version the surface showed (S-I2).
        if (command.version !== card.version) return { type: "changed", card };
        const said = command.answer === "deny" ? "You said no in the home app at 07:40" : "You said yes in the home app at 07:40";
        const decided = decidedCard(card, said, true);
        this.cards.set(card.id, decided);
        // The session's queued input is sent once nothing of it waits.
        if (!this.open(card.sessionId)) this.queues.delete(card.sessionId);
        return { type: "answered", card: decided };
      }
      case "send": {
        if (!this.open(command.sessionId)) return { type: "sent", sessionId: command.sessionId };
        const queue = [...(this.queues.get(command.sessionId) ?? []), command.text];
        this.queues.set(command.sessionId, queue);
        return { type: "queued", sessionId: command.sessionId, position: queue.length, text: "Sent after this card is answered" };
      }
      case "stopAll":
        this.stopped = true;
        return { type: "stopped", text: "All agents stopped" };
      case "resumeAll":
        this.stopped = false;
        return { type: "resumed" };
      default:
        throw new ApiError(400, "not in the fixture");
    }
  }

  async *feed(session: string, after: number | undefined, signal: AbortSignal): AsyncIterable<FeedMessage> {
    const entries = session === "s-finance" ? financeThread() : [];
    for (const entry of entries) if (entry.seq > (after ?? 0)) yield { type: "entry", entry };
    let shown = "";
    while (!signal.aborted) {
      const open = [...this.cards.values()].filter((card) => card.sessionId === session && card.status === "waiting");
      const signature = open.map((card) => `${card.id}:${card.version}`).join();
      if (signature !== shown) {
        shown = signature;
        yield { type: "decisions", cards: open };
      }
      await new Promise((resolve) => setTimeout(resolve, 50));
    }
  }

  private open(session: string) {
    return [...this.cards.values()].some((card) => card.sessionId === session && card.status === "waiting");
  }

  async queued(session: string) {
    return this.queues.get(session) ?? [];
  }

  /** Take over, as the engine answers it: the card is decided by the person. */
  async takeOver(card: DecisionCard): Promise<CommandOutcome> {
    const decided = decidedCard(card, "You typed it in the home app at 07:40", true);
    this.cards.set(card.id, decided);
    return { type: "answered", card: decided };
  }

  async processes() {
    return processes;
  }

  async agenda() {
    return this.agendaList;
  }

  async answerAgenda(id: string) {
    this.agendaList = this.agendaList.filter((item) => item.id !== id);
  }

  async addRoutine(target: string, text: string, wake: WakeSet) {
    processes.push({ run: { runId: "p-new", target, state: "waiting" }, meta: { pid: "p-new", wake, labels: { title: text } } });
  }

  async rules() {
    return this.ruleList;
  }

  async revokeRule(id: string) {
    this.ruleList = this.ruleList.filter((rule) => rule.id !== id);
  }

  async get<T>(): Promise<T> {
    throw new ApiError(404, "not in the fixture");
  }
}

function decidedCard(card: DecisionCard, said: string, byYou: boolean): DecisionCard {
  const answer = said.includes("said no") ? "no" : "yes";
  return {
    ...card,
    version: card.version + 1,
    status: "decided",
    statusText: "Decided",
    timeline: [...card.timeline.slice(0, -1).map((step) => ({ ...step, current: false })), { text: said, current: true }],
    allowed: { options: [], openInHome: false },
    decided: {
      answer,
      at: new Date().toISOString(),
      atText: said.slice(-5),
      by: byYou ? "You" : "You",
      byYou,
      text: said,
      via: byYou ? { surface: "web", origin: { kind: "pointer" } } : { surface: "chat", origin: { kind: "chat", channel: "Telegram" } },
    },
  };
}

/** What the example's seams were asked to do, for tests. */
export interface SeamCalls {
  seen: number[];
  records: number[];
  accepted: string[];
}

/** The seams of the example: what K2, K3, K4 and K5 serve. */
export function fixtureSeams(calls: SeamCalls = { seen: [], records: [], accepted: [] }): Seams {
  let page = awayPage();
  let settings = area();
  const change = (next: Partial<AreaSettings>) => {
    settings = { ...settings, ...next };
    return settings;
  };
  return {
    whileAway: {
      read: async () => page,
      markSeen: async (through) => {
        calls.seen.push(through);
        page = {
          ...page,
          since: new Date().toISOString(),
          after: through,
          counts: { done: 0, failed: 0, stopped: 0, suggestions: 0, questions: 0, decided: 0 },
          spentMicroUsd: 0,
          lines: [],
        };
      },
      record: async (seq) => {
        calls.records.push(seq);
        const line = awayPage().lines.find((candidate) => candidate.seq === seq);
        return {
          line,
          entry: { seq, ts: line?.at ?? new Date().toISOString(), partition: "default", agent: line?.agent ?? "kernel", kind: "process" },
        };
      },
    },
    memory: {
      facts: async () => ({
        tidyUp: { id: "tick-1", text: "Overnight, Finance merged two notes about your accountant." },
        facts: [
          { id: "f1", text: "You bill [CLIENT] at [RATE] an hour.", provenance: "youSaidThis" },
          { id: "f2", text: "Your accountant is [NAME].", provenance: "youConfirmedThis" },
          { id: "f3", text: "You send invoices on the last working day.", provenance: "aGuess", noticedBy: "Finance" },
        ],
      }),
      confirm: async () => {},
      fix: async () => {},
      forget: async () => {},
      undo: async () => {},
    },
    record: {
      check: async () => ({ intact: true, count: 1284, checkedAt: new Date().toISOString() }),
      recent: async () => [
        { at: at(7, 31), atText: "07:31", text: "You let Finance send one email, from Telegram." },
        { at: at(23, 30, -1), atText: "23:30", text: "You let Code run the tests once, from your phone." },
        { at: at(23, 5, -1), atText: "23:05", text: "Finance suggested a payment. Nothing was paid." },
      ],
    },
    devices: {
      list: async () => [
        { id: "d1", name: "Your phone", may: "asks for your face or fingerprint" },
        { id: "d2", name: "Telegram", may: "yes once, or no" },
      ],
      remove: async () => {},
      pair: async () => "482 913",
      waiting: async () =>
        calls.accepted.length > 0
          ? []
          : [
              {
                id: "telegram:4242",
                channel: "telegram",
                account: "4242",
                displayName: "Sam",
                requestedAt: at(7, 10),
                expiresAt: at(8, 10),
              },
            ],
      accept: async (code) => {
        if (code !== "ABCD-EFGH-2345") throw new ApiError(401, "that code is wrong, used or expired", "kernel.unauthorized");
        calls.accepted.push(code);
      },
    },
    limits: {
      areas: async () => [settings],
      area: async () => settings,
      setQuietTime: async (_, window) => change({ quietTime: window ? [window] : [] }),
      setSpendingLimit: async (_, microUsd) => change({ dailySpendingLimit: microUsd ?? undefined }),
      setMode: async (_, mode) => change({ mode }),
    },
  };
}

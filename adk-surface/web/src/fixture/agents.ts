/**
 * Agents a person adds over the example of spec 3.3 §5.7 (Finance, suggested by ADK-Rust),
 * held in memory, for component tests and for rendering the screens. It answers as the kernel's
 * agent table does: every change previews first and applies only with the preview's digest; a
 * change against an older version is out of date; removing names what ends. Never part of the
 * built page.
 */
import { createElement } from "react";
import type { ReactNode } from "react";
import type { AgentRef, AgentRow, DecisionCard, Origin } from "../generated/api";
import { App } from "../App";
import type { HomeClient } from "../client";
import { ApiError } from "../client";
import type { FixtureClient } from "./fixture";
import { agents as fixtureAgents, fixtureSeams } from "./fixture";
import type { StandingRule } from "../kernel";
import type { AgentsSeam } from "../agents/client";
import type {
  AgentCardView,
  AgentChoices,
  AgentCommand,
  AgentDetail,
  AgentDraft,
  AgentOutcome,
  AgentPreview,
  AgentPreviewRequest,
  ColorChoice,
  GrantedUse,
} from "../agents/types";

const PALETTE: ColorChoice[] = [
  { color: "orange", label: "Orange", hex: "#C03A00" },
  { color: "green", label: "Green", hex: "#15803D" },
  { color: "blue", label: "Blue", hex: "#1D4ED8" },
  { color: "violet", label: "Violet", hex: "#7C3AED" },
  { color: "slate", label: "Slate", hex: "#475569" },
  { color: "amber", label: "Amber", hex: "#B45309" },
  { color: "teal", label: "Teal", hex: "#0E7490" },
  { color: "pink", label: "Pink", hex: "#BE185D" },
];

const MODE_WORDS = {
  justLook: { label: "Just look", status: "Just looks" },
  suggest: { label: "Suggest", status: "Suggests" },
  askFirst: { label: "Ask first", status: "Asks first" },
  goAhead: { label: "Go ahead", status: "Goes ahead" },
} as const;

export function fixtureChoices(): AgentChoices {
  return {
    models: [
      { provider: "anthropic", model: "claude-sonnet-5-5" },
      { provider: "openai", model: "gpt-5.6-terra" },
    ],
    packs: ["browser"],
    areas: [
      { id: "default", name: "Home", mode: { mode: "askFirst", ...MODE_WORDS.askFirst }, drivers: ["calendar"] },
      { id: "work", name: "Work", mode: { mode: "goAhead", ...MODE_WORDS.goAhead }, drivers: [] },
    ],
    colors: PALETTE,
  };
}

export function financeDraft(): AgentDraft {
  return {
    displayName: "Finance",
    color: "green",
    description: "Reads your invoices and drafts payment reminders",
    instructions: "Read the invoices in Invoices. Draft a polite reminder for each one overdue. Never send them.",
    area: "default",
    mode: "askFirst",
    model: { provider: "anthropic", model: "claude-sonnet-5-5" },
    uses: { files: "read", folder: "Invoices", shell: "none", web: { access: "none" }, packs: [], drivers: [] },
    dailyLimitMicroUsd: null,
  };
}

/** The abilities and risk of `draft`, by the risk table of spec 3.3 §5.7. */
export function assess(draft: AgentDraft): Pick<AgentPreview, "abilities" | "risk" | "riskText" | "money"> {
  const abilities: string[] = [];
  let risk = draft.mode === "goAhead" ? 2 : draft.mode === "askFirst" ? 1 : 0;
  abilities.push(
    {
      justLook: "It only looks: it changes nothing.",
      suggest: "It suggests what to do and changes nothing until you say yes.",
      askFirst: "It asks you first before it changes anything.",
      goAhead: "It goes ahead without asking, except for what always asks first.",
    }[draft.mode],
  );
  const place = draft.uses.folder || "the workspace";
  if (draft.uses.files === "none") abilities.push("It can't use your files.");
  if (draft.uses.files === "read") abilities.push(`It can read files in ${place}.`);
  if (draft.uses.files === "readWrite") {
    risk = Math.max(risk, 1);
    abilities.push(`It can read and change files in ${place}.`);
  }
  if (draft.uses.shell === "none") abilities.push("It has no shell.");
  if (draft.uses.shell === "sandboxed") {
    risk = Math.max(risk, 1);
    abilities.push("It can run commands in the sandbox.");
  }
  if (draft.uses.shell === "host") {
    risk = 2;
    abilities.push("It can run commands on this computer.");
  }
  if (draft.uses.web.access === "none") abilities.push("It can't use the web.");
  if (draft.uses.web.access === "listed") abilities.push(`It can use these sites only: ${draft.uses.web.hosts.join(", ")}.`);
  if (draft.uses.web.access === "any") {
    risk = Math.max(risk, 1);
    abilities.push("It can use any site.");
  }
  for (const pack of draft.uses.packs) abilities.push(`It can use the ${pack} pack.`);
  for (const driver of draft.uses.drivers) abilities.push(`It can use the ${driver} driver.`);
  const level = (["low", "medium", "high"] as const)[risk];
  return { abilities, risk: level, riskText: `${level[0].toUpperCase()}${level.slice(1)} risk`, money: false };
}

function digestOf(value: unknown): string {
  let hash = 0x811c9dc5;
  for (const byte of new TextEncoder().encode(JSON.stringify(value))) hash = Math.imul(hash ^ byte, 0x01000193) >>> 0;
  return hash.toString(16).padStart(8, "0").repeat(8);
}

function idFor(name: string, taken: Set<string>): string {
  const base = name.toLowerCase().replace(/[^a-z0-9]+/g, "-").replace(/^-+|-+$/g, "").slice(0, 40) || "agent";
  if (!taken.has(base)) return base;
  for (let n = 2; ; n += 1) if (!taken.has(`${base}-${n}`)) return `${base}-${n}`;
}

const ADK_RUST: AgentRef = { name: "adk-rust", displayName: "ADK-Rust", avatar: { monogram: "A", color: "#C03A00" } };

/**
 * The suggestion of `draft` (Finance by default) by `suggester` as a decision card (spec 3.3 §5.7
 * "The decision that adds an agent").
 */
export function suggestionCard(
  draft = financeDraft(),
  suggester = ADK_RUST,
  sessionId = "s-chief",
): DecisionCard & { addsAgent: AgentCardView } {
  const name = draft.displayName;
  const id = name.toLowerCase().replace(/[^a-z0-9]+/g, "-");
  const by = suggester.displayName;
  const files = { none: "No files", read: `Reads files in ${draft.uses.folder || "the workspace"}`, readWrite: `Reads and changes files in ${draft.uses.folder || "the workspace"}` };
  const web = draft.uses.web.access === "listed" ? `These sites only: ${draft.uses.web.hosts.join(", ")}` : draft.uses.web.access === "any" ? "Any site" : "No web";
  const assessed = assess(draft);
  return {
    id: `agent-${id}-3f9a2c0d11aa`,
    version: 1,
    sessionId,
    agent: suggester,
    waitingSince: new Date(Date.now() - 2 * 60_000).toISOString(),
    sinceText: "since 09:12",
    status: "waiting",
    statusText: "Waiting for you",
    heading: `Add an agent called ${name}`,
    call: {
      tool: "kernel:agent.create",
      args: { displayName: name },
      text: `kernel:agent.create(displayName: "${name}")`,
      truncated: false,
    },
    timeline: [
      { text: "You asked at 09:10", current: false },
      { text: `${by} read 2 files`, current: false },
      { text: `${by} suggested this agent`, current: false },
      { text: "Waiting for you", current: true },
    ],
    allowed: {
      options: [
        { answer: "allowOnce", label: "Add this agent", needsExpanded: false, needsPresence: false },
        { answer: "deny", label: "Don't add", needsExpanded: false, needsPresence: false },
      ],
      openInHome: false,
    },
    risk: assessed.risk,
    riskText: assessed.riskText,
    reversibility: "cantBeUndone",
    reversibilityText: "You can remove it later",
    money: false,
    alsoShown: "Also on your phone",
    addsAgent: {
      id,
      agent: {
        name: id,
        displayName: name,
        avatar: { monogram: name.slice(0, 1).toUpperCase(), color: PALETTE.find((choice) => choice.color === draft.color)?.hex ?? "#475569" },
      },
      description: draft.description,
      lines: [
        { label: "Area", value: "Home" },
        { label: "How much it asks first", value: MODE_WORDS[draft.mode].status },
        { label: "Runs on", value: draft.model?.model ?? "claude-sonnet-5-5", mono: true },
        { label: "Files", value: files[draft.uses.files] },
        { label: "Shell", value: "No shell" },
        { label: "Web", value: web },
      ],
      instructions: draft.instructions,
      instructionsTruncated: false,
      abilities: assessed.abilities,
      compared: draft.uses.web.access === "none" ? `It can do no more than ${by}, and has no shell or web.` : `It can do no more than ${by}, and has no shell.`,
      allowedWithoutAsking: "Nothing yet",
      draft,
    },
  };
}

/** A second suggestion for the home's screenshot, beside the fixture's own Finance. */
function travelDraft(): AgentDraft {
  return {
    ...financeDraft(),
    displayName: "Travel",
    color: "teal",
    description: "Finds train times and drafts trip plans",
    instructions: "Look up train times on the listed sites. Draft a plan for each trip in the calendar. Never book anything.",
    uses: { files: "none", folder: "", shell: "none", web: { access: "listed", hosts: ["[RAIL SITE]"] }, packs: [], drivers: [] },
  };
}

/** The roster row of an added agent, as the engine marks it. */
export type AddedRow = AgentRow & { added?: boolean; canChange?: boolean; area?: string };

/** An in-memory agent table over the example. */
export class FixtureAgents implements AgentsSeam {
  readonly taken: { command: AgentCommand; origin: Origin }[] = [];
  readonly details = new Map<string, AgentDetail>();
  private removed = new Set<string>();
  private ruleList: StandingRule[] = [
    { id: "r-read", tool: "read_file", label: "Reading files in this workspace", decision: { kind: "allow" }, createdAt: "2026-10-20T09:00:00Z" },
  ];
  /** Work that removing an agent ends, by agent. */
  readonly running: Record<string, { work: number; decisions: number; routines: number }> = {};

  /** `granted`: what the home grants every agent it runs, as `SurfaceEngineBuilder::grant` adds it. */
  constructor(
    withFinance = true,
    private readonly granted: GrantedUse[] = [],
  ) {
    if (withFinance) this.put("finance", financeDraft(), "Suggested by ADK-Rust, added by you on 25 Oct");
  }

  private put(id: string, draft: AgentDraft, originText: string, version = 1): AgentDetail {
    const color = PALETTE.find((choice) => choice.color === draft.color)?.hex ?? "#475569";
    const detail: AgentDetail = {
      id,
      agent: { name: id, displayName: draft.displayName, avatar: { monogram: draft.displayName.slice(0, 1).toUpperCase(), color } },
      draft,
      version,
      state: this.details.get(id)?.state ?? "active",
      originText,
      mode: { mode: draft.mode, ...MODE_WORDS[draft.mode] },
      cantStart: null,
      changedAt: new Date().toISOString(),
      ...(this.granted.length > 0 ? { granted: this.granted } : {}),
    };
    this.details.set(id, detail);
    return detail;
  }

  /** The roster rows of the added agents, as the engine marks them: area by name, own mode. */
  rows(): AddedRow[] {
    return [...this.details.values()].map((detail) => ({
      agent: detail.agent,
      status: "done",
      statusText: detail.state === "paused" ? "Paused" : "Ready",
      added: true,
      canChange: true,
      area: fixtureChoices().areas.find((area) => area.id === detail.draft.area)?.name ?? detail.draft.area,
      mode: detail.mode,
    }));
  }

  async agent(id: string) {
    const detail = this.details.get(id);
    if (!detail) throw new ApiError(404, `no agent '${id}'`, "surface.not_found");
    return detail;
  }

  async choices() {
    return fixtureChoices();
  }

  async rules() {
    return this.ruleList;
  }

  async copyRule(agent: string, rule: string, origin: Origin) {
    await this.command({ type: "copyRuleToAgent", agent, ruleId: rule }, origin);
  }

  async preview(request: AgentPreviewRequest): Promise<AgentPreview> {
    if (request.action === "remove") {
      const detail = await this.agent(request.agent);
      const running = this.running[request.agent] ?? { work: 0, decisions: 0, routines: 0 };
      const parts = [
        running.decisions ? `${running.decisions} decision${running.decisions === 1 ? "" : "s"} waiting for you` : "",
        running.routines ? `${running.routines} routine${running.routines === 1 ? "" : "s"}` : "",
      ].filter(Boolean);
      const name = detail.draft.displayName;
      const stops = running.work ? `stops ${running.work} piece${running.work === 1 ? "" : "s"} of work` : "";
      const ends = parts.length ? `ends ${parts.join(" and ")}` : "";
      const body = [stops, ends].filter(Boolean).join(", ") || "ends nothing that is running";
      return {
        id: request.agent,
        action: "remove",
        heading: `Here is what ${name} can do`,
        abilities: [],
        risk: "low",
        riskText: "Low risk",
        money: false,
        widens: false,
        needsPresence: false,
        ended: { ...running, followUps: 0, rules: 0, text: `Removing ${name} ${body}. Its threads stay.` },
        digest: digestOf(request),
        code: digestOf(request).slice(0, 6),
      };
    }
    const draft = request.draft;
    if (!draft.displayName.trim()) throw new ApiError(400, "an agent's name is 1 to 40 characters on one line", "surface.invalid");
    const assessed = assess(draft);
    const was = request.action === "change" ? (await this.agent(request.agent)).draft : undefined;
    const fields = was ? Object.keys(draft).filter((key) => JSON.stringify(was[key as keyof AgentDraft]) !== JSON.stringify(draft[key as keyof AgentDraft])) : [];
    const taken = new Set([...this.details.keys(), ...this.removed]);
    return {
      id: request.action === "change" ? request.agent : idFor(draft.displayName, taken),
      action: request.action,
      heading: `Here is what ${draft.displayName} can do`,
      ...assessed,
      widens: request.action === "create" || fields.includes("mode") || fields.includes("uses"),
      needsPresence: assessed.risk === "high",
      fields,
      was: was ?? null,
      now: was ? draft : null,
      digest: digestOf(request),
      code: digestOf(request).slice(0, 6),
    };
  }

  async command(command: AgentCommand, origin: Origin): Promise<AgentOutcome> {
    this.taken.push({ command, origin });
    switch (command.type) {
      case "newThread":
        return { type: "sent", sessionId: command.sessionId };
      case "addAgent": {
        const preview = await this.preview({ action: "create", draft: command.draft, replaces: command.replaces });
        if (preview.digest !== command.digest) throw new ApiError(409, "the definition changed since it was shown", "surface.changed");
        const detail = this.put(preview.id, command.draft, "Added by you on 25 Oct");
        return { type: "agentAdded", agent: detail };
      }
      case "changeAgent": {
        const current = await this.agent(command.agent);
        if (current.version !== command.version) {
          return { type: "agentOutOfDate", agent: current, text: `${current.draft.displayName} was changed in the terminal at 10:01` };
        }
        return { type: "agentChanged", agent: this.put(command.agent, command.draft, current.originText, current.version + 1) };
      }
      case "pauseAgent":
      case "resumeAgent": {
        const current = await this.agent(command.agent);
        const next: AgentDetail = { ...current, state: command.type === "pauseAgent" ? "paused" : "active" };
        this.details.set(command.agent, next);
        return { type: command.type === "pauseAgent" ? "agentPaused" : "agentResumed", agent: next };
      }
      case "removeAgent": {
        const preview = await this.preview({ action: "remove", agent: command.agent, version: command.version });
        this.details.delete(command.agent);
        this.removed.add(command.agent);
        return { type: "agentRemoved", ended: preview.ended! };
      }
      case "copyRuleToAgent": {
        const rule = this.ruleList.find((candidate) => candidate.id === command.ruleId);
        return { type: "ruleCopied", agent: command.agent, label: rule?.label ?? "" };
      }
    }
  }
}

/**
 * `client` with agents a person added: Finance in the roster (over the fixture's own Finance row),
 * New agent, and `suggestion` waiting in Decisions.
 */
export function withAgents(client: FixtureClient, agents = new FixtureAgents(), suggestion = suggestionCard()): HomeClient {
  const rows = client.agents.bind(client);
  const home = client as FixtureClient & HomeClient;
  home.agents = async () => {
    const added = agents.rows();
    const base = (await rows()).map((row) => {
      const mine = added.find((candidate) => candidate.agent.name === row.agent.name);
      return mine ? { ...row, added: true, canChange: true, area: mine.area, mode: mine.mode } : row;
    });
    return [...base, ...added.filter((row) => !base.some((candidate) => candidate.agent.name === row.agent.name))];
  };
  home.agentsSeam = () => agents;
  home.agentsWithPrimary = async () => ({ agents: await home.agents(), primary: ADK_RUST, canAdd: true });
  client.hold(suggestion);
  return home;
}

/**
 * The agents screens for `npm run dev` and the screenshots, in the home: `?screen=agents&agents=new`
 * (New agent), `=settings` (Finance's settings), `=home` (the roster with New agent, and Travel
 * suggested by the Chief of staff in its thread).
 */
export function agentsFixture(params: URLSearchParams, client: FixtureClient): ReactNode {
  const view = params.get("agents") ?? "new";
  const path = { new: "/ui/agents/new", settings: "/ui/agents/finance" }[view as "new" | "settings"];
  if (path) window.history.replaceState(null, "", `${path}${window.location.search}`);
  const suggestion = path ? suggestionCard() : suggestionCard(travelDraft(), fixtureAgents.chief);
  // `&granted`: the home grants every agent drawing forms and changes on the screens (spec 3.3 §5.7).
  const granted = params.has("granted") ? fixtureGrants() : [];
  return createElement(App, {
    client: withAgents(client, new FixtureAgents(true, granted), suggestion),
    seams: fixtureSeams(),
    initialSection: path ? "agents" : "threads",
  });
}

/** What a home with `genui` and `verb-ui` grants every agent it runs (spec 3.3 §5.7). */
export function fixtureGrants(): GrantedUse[] {
  return [
    { id: "genui", text: "Show forms and tables in your threads — sending asks you first" },
    { id: "verb-ui", text: "Ask to change things on your screens — always asks you first", modes: ["askFirst", "goAhead"] },
  ];
}

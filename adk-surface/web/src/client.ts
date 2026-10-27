/**
 * The web home's client: the engine's routes under `/api/surface` (spec 3.3 §6), and the
 * kernel and harness routes beside them. Every read is for one surface (`surface=web` unless
 * another shell names its own); every command carries the surface and how the input was given
 * (§11).
 */
import type {
  AgentRow,
  AgentsResponse,
  AppAnswer,
  AppMessageRequest,
  AppOpen,
  Briefing,
  Command,
  CommandOutcome,
  DecisionCard,
  DecisionsResponse,
  FeedEntry,
  Origin,
  PhoneDevicesResponse,
  QueuedResponse,
  StatusLine,
  SurfaceKind,
  TakeOverRequest,
  VoiceModels,
  VoiceTicket,
  VoiceTicketRequest,
} from "./generated/api";
import type { AgentsSeam } from "./agents/client";
import { httpAgents } from "./agents/client";
import type { AppsSeam } from "./genui/apps";
import { appPath } from "./genui/apps";
import type { TakeOverSeam } from "./components/DecisionCard";
import { mergeAway } from "./away";
import type {
  AgendaItem,
  AreaMode,
  AreaSettings,
  AwayPage,
  AwayRecord,
  ChannelAccount,
  ChannelWaiting,
  KernelDevice,
  KernelProcess,
  KernelRecordCheck,
  KernelRecordLine,
  MemoryResponse,
  QuietWindow,
  StandingRule,
  TidyUpUndo,
  WakeSet,
} from "./kernel";
import { accountName, channelName, devicePowers } from "./kernel";
import { labels } from "./labels";
import { memoryOf, undoOutcome } from "./memory";
import { phoneLabels } from "./phone/labels";
import { confirmAnswer, platformPasskeys, registerPasskey } from "./phone/passkey";
import { confirmAgentChange } from "./agents/confirm";
import type { AgentCommand, AgentOutcome } from "./agents/types";
import type {
  AgentSpend,
  DevicesSeam,
  LimitsSeam,
  MemorySeam,
  PairingIssued,
  RecordSeam,
  RosterSeam,
  WhileAwaySeam,
} from "./seams";
import { parseSse } from "./sse";
import { readStored, writeStored } from "./stored";

/** Where this tab keeps the server's token, when the server asks for one. */
const TOKEN_KEY = "adk.home.token";

/**
 * The name the home reads "While you were away" as (spec 3.3 §10 K4). A paired device reads as
 * itself whatever it names, so each phone keeps its own place.
 */
export const VIEWER = "home";

/** At most this many pages of "While you were away" are read at once, 2,000 rows each. */
const AWAY_PAGES = 10;

/**
 * The agenda states the home reads: an item open for an answer, or on its way to being done.
 * Every store lists the oldest items first and keeps finished ones, so one read of every state
 * would end at the oldest items and miss the new ones (spec 3.3 §5.1).
 */
const LIVE_AGENDA = ["proposed", "approved", "scheduled", "running"] as const;

/** At most this many agenda items of one state are read at once. */
const AGENDA_LIMIT = 200;

/** A problem document from the server, with its `surface.*`, `approval.*` or other code. */
export class ApiError extends Error {
  constructor(
    readonly status: number,
    message: string,
    readonly code?: string,
  ) {
    super(message);
  }
}

/**
 * What went wrong, in the page's words: a server's problem document says itself; a request
 * that never reached the server is "Can't reach your agents".
 */
export function problemText(error: unknown): string {
  return error instanceof ApiError ? error.message : labels.cantReach;
}

/** One frame of a session's feed as it arrives (spec 3.3 §6). */
export type FeedMessage =
  | { type: "entry"; entry: FeedEntry }
  | { type: "decisions"; cards: DecisionCard[] }
  | { type: "reset"; len: number };

/** What the screens read and do. Tests and previews give their own. */
export interface HomeClient {
  /** The status line, with the mode `session` runs in when given, else the agents' default. */
  status(session?: string): Promise<StatusLine>;
  agents(): Promise<AgentRow[]>;
  decisions(): Promise<DecisionCard[]>;
  decision(id: string): Promise<DecisionCard>;
  briefing(since?: string): Promise<Briefing>;
  command(command: Command, origin: Origin): Promise<CommandOutcome>;
  /** The session's feed from just after `after`, until `signal` aborts or the server closes. */
  feed(session: string, after: number | undefined, signal: AbortSignal): AsyncIterable<FeedMessage>;
  /** What the caller typed while a decision of `session` was open, sent once it is answered. */
  queued(session: string): Promise<string[]>;
  /** The kernel's process table; `undefined` without a kernel. */
  processes(): Promise<KernelProcess[] | undefined>;
  /** The kernel's agenda items open for an answer or on their way to being done; `undefined` without one. */
  agenda(): Promise<AgendaItem[] | undefined>;
  /** Approves or rejects an agenda item; resolves to the item after the answer. */
  answerAgenda(id: string, yes: boolean): Promise<AgendaItem | undefined>;
  /** Starts a sleeper of `target` that wakes on `wake` and runs `text`. */
  addRoutine(target: string, text: string, wake: WakeSet): Promise<void>;
  /** The caller's standing rules; `undefined` without a harness. */
  rules(): Promise<StandingRule[] | undefined>;
  revokeRule(id: string): Promise<void>;
  /**
   * "Extend" (spec 3.3 §8.1): moves the end of a rule with an end date to `until`, an ISO
   * time later than its end; resolves to the rule after it.
   */
  extendRule?(id: string, until: string): Promise<StandingRule>;
  /** Any other JSON route, for the developer panel. */
  get<T>(path: string): Promise<T>;
  /** Sends the server's token from now on, for a server that asks for one. */
  signIn?(token: string): void;
  /** The models voice can talk on; `undefined` when the server does not offer voice (spec 3.3 §5.6). */
  voiceModels?(): Promise<VoiceModels | undefined>;
  /** A single-use ticket for the voice socket, continuing the voice session `sessionId` when given. */
  voiceTicket?(model: string, sessionId?: string): Promise<VoiceTicket>;
  /** The open decisions with the answers voice may give them. */
  voiceDecisions?(): Promise<DecisionCard[]>;
  /** A command given on the voice view: surface voice. */
  voiceCommand?(command: Command, origin: Origin): Promise<CommandOutcome>;
  /** The agents a person adds (spec 3.3 §5.7): their reads, previews and changes. */
  agentsSeam?(): AgentsSeam;
  /** Makes a high-risk agent change after "Confirm it's you" with this computer's passkey. */
  confirmAgentChange?(command: AgentCommand): Promise<AgentOutcome>;
  /** The roster with the primary agent and whether agents can be added here. */
  agentsWithPrimary?(): Promise<AgentsResponse>;
  /** Apps from MCP servers in a thread (spec 3.3 §6.1 `genui-apps`): opening them and relaying what they ask. */
  apps?(): AppsSeam;
}

async function problem(response: Response): Promise<ApiError> {
  const text = await response.text();
  try {
    const value = JSON.parse(text) as {
      detail?: string;
      title?: string;
      message?: string;
      code?: string;
      // The server's problem document (`AdkError::to_problem_json`).
      error?: { message?: string; code?: string };
    };
    return new ApiError(
      response.status,
      value.error?.message ?? value.detail ?? value.message ?? value.title ?? `${response.status} ${response.statusText}`,
      value.error?.code ?? value.code,
    );
  } catch {
    return new ApiError(response.status, text || `${response.status} ${response.statusText}`);
  }
}

/**
 * The client over HTTP, relative to the page's own server. A server started with a token
 * (`adk-rust serve --auth-token`) needs it on every request: `signIn` keeps it for this
 * tab and sends it as `Authorization: Bearer`.
 *
 * `surface` is the surface the engine computes answers for and stamps on every command
 * (spec 3.3 §7.2, §11): the web home by default; the tray and the phone shells name their own.
 */
export class HttpClient implements HomeClient {
  private token: string | undefined = readStored<string | undefined>(TOKEN_KEY, undefined);

  constructor(
    private readonly base = "/api",
    readonly surface: SurfaceKind = "web",
  ) {}

  /** Sends `token` with every request from now on, and keeps it for this tab. */
  signIn(token: string) {
    this.token = token.trim() || undefined;
    writeStored(TOKEN_KEY, this.token);
  }

  private headers(extra?: HeadersInit): Record<string, string> {
    return {
      ...(this.token ? { authorization: `Bearer ${this.token}` } : {}),
      ...(extra as Record<string, string> | undefined),
    };
  }

  private async request<T>(path: string, init?: RequestInit): Promise<T> {
    const response = await fetch(`${this.base}${path}`, {
      credentials: "same-origin",
      ...init,
      headers: this.headers({
        accept: "application/json",
        ...(init?.body ? { "content-type": "application/json" } : {}),
        ...(init?.headers as Record<string, string> | undefined),
      }),
    });
    if (!response.ok) throw await problem(response);
    if (response.status === 204) return undefined as T;
    const text = await response.text();
    return (text ? JSON.parse(text) : undefined) as T;
  }

  /** A route the server may not mount: `undefined` on 404. */
  private async optional<T>(path: string): Promise<T | undefined> {
    try {
      return await this.request<T>(path);
    } catch (error) {
      if (error instanceof ApiError && (error.status === 404 || error.status === 405)) return undefined;
      throw error;
    }
  }

  private post<T>(path: string, body: unknown): Promise<T> {
    return this.request<T>(path, { method: "POST", body: JSON.stringify(body) });
  }

  status(session?: string) {
    const query = session ? `&session=${encodeURIComponent(session)}` : "";
    return this.request<StatusLine>(`/surface/status?surface=${this.surface}${query}`);
  }

  async agents() {
    return (await this.request<AgentsResponse>(`/surface/agents?surface=${this.surface}`)).agents;
  }

  /** The agents with the one a message goes to when the person names none (spec 3.3 §5.5). */
  agentsWithPrimary() {
    return this.request<AgentsResponse>(`/surface/agents?surface=${this.surface}`);
  }

  /** "Confirm it's you" for an agent change at high risk (spec 3.3 §7.2). */
  confirmAgentChange(command: AgentCommand): Promise<AgentOutcome> {
    return confirmAgentChange(this.post.bind(this), command);
  }

  /** The agents a person adds (spec 3.3 §5.7), over the engine's reads and commands. */
  apps(): AppsSeam {
    return {
      open: (session, call) => this.optional<AppOpen>(`${appPath(session, call)}?surface=${this.surface}`),
      request: (session, call, request) => this.post<AppAnswer>(`${appPath(session, call)}/requests`, request),
      message: (session, call, message) =>
        this.post<CommandOutcome>(`${appPath(session, call)}/message`, { message } satisfies AppMessageRequest),
    };
  }

  agentsSeam(): AgentsSeam {
    return httpAgents({
      get: (path) => this.request(path),
      post: (path, body) => this.post(path, body),
      command: (command, origin) => this.command(command as Command, origin),
      rules: () => this.rules(),
    });
  }

  async decisions() {
    return (await this.request<DecisionsResponse>(`/surface/decisions?surface=${this.surface}`)).decisions;
  }

  decision(id: string) {
    return this.request<DecisionCard>(`/surface/decisions/${encodeURIComponent(id)}?surface=${this.surface}`);
  }

  briefing(since?: string) {
    const query = since ? `&since=${encodeURIComponent(since)}` : "";
    return this.request<Briefing>(`/surface/briefing?surface=${this.surface}${query}`);
  }

  async command(command: Command, origin: Origin) {
    try {
      return await this.request<CommandOutcome>("/surface/commands", {
        method: "POST",
        body: JSON.stringify({ surface: this.surface, origin, command }),
      });
    } catch (error) {
      // A high-risk answer goes through "Confirm it's you" with the person's passkey on this
      // computer (spec 3.3 §8.2); a server without the phone routes keeps its refusal.
      if (command.type !== "decide" || !(error instanceof ApiError) || error.code !== "surface.confirm_presence") {
        throw error;
      }
      const request = {
        version: command.version,
        answer: command.answer,
        expanded: command.expanded ?? false,
        ...(command.editedArgs === undefined ? {} : { editedArgs: command.editedArgs }),
      };
      return confirmAnswer(this.post.bind(this), command.decisionId, request).catch((failure: unknown) => {
        throw failure instanceof ApiError && (failure.status === 404 || failure.status === 405) ? error : failure;
      });
    }
  }

  async *feed(session: string, after: number | undefined, signal: AbortSignal): AsyncIterable<FeedMessage> {
    const headers = this.headers({ accept: "text/event-stream" });
    if (after !== undefined) headers["last-event-id"] = String(after);
    const response = await fetch(
      `${this.base}/surface/sessions/${encodeURIComponent(session)}/feed?surface=${this.surface}`,
      { headers, signal, credentials: "same-origin" },
    );
    if (!response.ok) throw await problem(response);
    if (!response.body) throw new ApiError(response.status, "the feed sent no stream");
    for await (const frame of parseSse(response.body)) {
      if (!frame.data) continue;
      const data = JSON.parse(frame.data) as unknown;
      switch (frame.event) {
        case "entry":
          yield { type: "entry", entry: data as FeedEntry };
          break;
        case "decisions":
          yield { type: "decisions", cards: data as DecisionCard[] };
          break;
        case "reset":
          yield { type: "reset", len: (data as { len: number }).len };
          break;
        case "error": {
          const value = data as { detail?: string; title?: string; code?: string };
          throw new ApiError(500, value.detail ?? value.title ?? "the feed failed", value.code);
        }
        default:
          break;
      }
    }
  }

  async queued(session: string) {
    const page = await this.request<QueuedResponse>(
      `/surface/sessions/${encodeURIComponent(session)}/queued?surface=${this.surface}`,
    );
    return page.queued;
  }

  /**
   * "Take over, then Done" (spec 3.3 §8.2, §10 H2): the secret the person typed goes to the
   * call that asked, for the card at the version shown.
   */
  takeOver(): TakeOverSeam {
    return {
      provide: (card, secret) =>
        this.request<CommandOutcome>(`/surface/decisions/${encodeURIComponent(card.id)}/take-over`, {
          method: "POST",
          body: JSON.stringify({ surface: this.surface, version: card.version, secret } satisfies TakeOverRequest),
        }),
    };
  }

  /**
   * Every waiting process, for Coming up. The kernel lists the oldest first and cuts the list at
   * a limit, so a limit would leave out the routines added last; waiting processes are what is
   * asleep now, not the kernel's history, so the whole list is read.
   */
  async processes() {
    const page = await this.optional<{ processes: KernelProcess[] }>("/kernel/processes?state=waiting");
    return page?.processes;
  }

  /** What is working now: the kernel's running processes (spec 3.3 §5.5); `undefined` without a kernel. */
  async running() {
    const page = await this.optional<{ processes: KernelProcess[] }>("/kernel/processes?state=running&limit=50");
    return page?.processes;
  }

  /**
   * One wake-up per frame of the kernel's event stream (`GET /api/kernel/events`), until
   * `signal` aborts or the stream ends, so the tray changes within one event (spec 3.3 §12).
   */
  async *events(signal: AbortSignal): AsyncIterable<void> {
    const response = await fetch(`${this.base}/kernel/events`, {
      headers: this.headers({ accept: "text/event-stream" }),
      signal,
      credentials: "same-origin",
    });
    if (!response.ok) throw await problem(response);
    if (!response.body) return;
    for await (const frame of parseSse(response.body)) {
      void frame;
      yield;
    }
  }

  async agenda() {
    const pages = await Promise.all(
      LIVE_AGENDA.map((state) =>
        this.optional<{ items: AgendaItem[] }>(`/kernel/agenda?state=${state}&limit=${AGENDA_LIMIT}`),
      ),
    );
    if (pages.every((page) => page === undefined)) return undefined;
    return pages.flatMap((page) => page?.items ?? []);
  }

  async answerAgenda(id: string, yes: boolean) {
    const answer = await this.request<{ item?: AgendaItem } | undefined>(
      `/kernel/agenda/${encodeURIComponent(id)}/${yes ? "approve" : "reject"}`,
      { method: "POST", body: "{}" },
    );
    return answer?.item;
  }

  async addRoutine(target: string, text: string, wake: WakeSet) {
    await this.request("/kernel/processes", {
      method: "POST",
      body: JSON.stringify({ target, input: { text }, kind: "scheduled", queue: "background", wake, labels: { title: text } }),
    });
  }

  async rules() {
    const page = await this.optional<{ rules: StandingRule[] }>("/rules");
    return page?.rules;
  }

  async revokeRule(id: string) {
    await this.request(`/rules/${encodeURIComponent(id)}`, { method: "DELETE" });
  }

  async extendRule(id: string, until: string) {
    const answer = await this.post<{ rule: StandingRule }>(`/rules/${encodeURIComponent(id)}/extend`, {
      expiresAt: until,
    });
    return answer.rule;
  }

  get<T>(path: string) {
    return this.request<T>(path);
  }

  voiceModels() {
    return this.optional<VoiceModels>("/surface/voice");
  }

  voiceTicket(model: string, sessionId?: string) {
    return this.post<VoiceTicket>("/surface/voice/sessions", { model, sessionId } satisfies VoiceTicketRequest);
  }

  voiceDecisions() {
    return new HttpClient(this.base, "voice").decisions();
  }

  voiceCommand(command: Command, origin: Origin) {
    return new HttpClient(this.base, "voice").command(command, origin);
  }

  /**
   * Where you can say yes from (spec 3.3 §10 K1, K2, §8.3): the kernel's paired devices and chat
   * accounts, removed and added through its pairing routes, with what each phone confirms with
   * and the passkeys on this computer when the server serves the phone.
   */
  devices(): DevicesSeam {
    // The phone routes answer 404 without the `mobile` feature and 400 without a home URL.
    const phones = () => this.optional<PhoneDevicesResponse>("/surface/phone/devices").catch(() => undefined);
    return {
      list: async () => {
        const paired = await this.optional<{ devices: KernelDevice[] }>("/kernel/devices");
        if (!paired) return undefined;
        const channels = await this.optional<{ accounts: ChannelAccount[] }>("/kernel/channels");
        const phone = await phones();
        const confirms = new Set(phone?.devices.filter((device) => device.passkey).map((device) => device.deviceId));
        return [
          ...paired.devices
            .filter((device) => device.state === "active")
            .map((device) => ({
              id: `device:${device.id}`,
              name: device.name,
              may: confirms.has(device.id) ? phoneLabels.asksForFaceOrFingerprint : devicePowers(device.scopes),
            })),
          ...(phone?.computerPasskeys ?? []).map((passkey) => ({
            id: `passkey:${passkey.id}`,
            name: phoneLabels.passkeyOnThisComputer,
            may: phoneLabels.asksForFaceOrFingerprint,
          })),
          ...(channels?.accounts ?? [])
            .filter((account) => account.state === "active")
            .map((account) => ({
              id: `channel:${account.id}`,
              name: `${channelName(account.channel)}: ${accountName(account)}`,
              may: labels.yesOnceOrNo,
            })),
        ];
      },
      remove: async (id: string) => {
        const [kind, rest] = [id.slice(0, id.indexOf(":")), id.slice(id.indexOf(":") + 1)];
        if (kind === "passkey") {
          await this.request(`/surface/phone/passkey/${encodeURIComponent(rest)}`, { method: "DELETE" });
          return;
        }
        const path = kind === "channel" ? "/kernel/channels" : "/kernel/devices";
        await this.request(`${path}/${encodeURIComponent(rest)}/revoke`, { method: "POST", body: "{}" });
      },
      // A phone, so its link opens the installed app and it may set up its passkey (§8.3).
      pair: async () => {
        const issued = await this.post<PairingIssued>("/kernel/pairing", { name: phoneLabels.yourPhone, kind: "phone" });
        return { code: issued.code, url: issued.url, expiresAt: issued.expiresAt };
      },
      qr: async (url: string) => {
        const response = await fetch(`${this.base}/surface/phone/qr`, {
          method: "POST",
          credentials: "same-origin",
          headers: this.headers({ accept: "image/svg+xml", "content-type": "application/json" }),
          body: JSON.stringify({ url }),
        });
        if (!response.ok) return undefined;
        return `data:image/svg+xml;charset=utf-8,${encodeURIComponent(await response.text())}`;
      },
      canAddPasskey: async () => {
        const phone = await phones();
        return phone !== undefined && phone.computerPasskeys.length === 0 && (await platformPasskeys());
      },
      addPasskey: () => registerPasskey(this.post.bind(this)),
      waiting: async () => (await this.optional<{ waiting?: ChannelWaiting[] }>("/kernel/channels"))?.waiting ?? [],
      accept: async (code: string) =>
        (await this.post<{ account?: ChannelAccount } | undefined>("/kernel/channels/pair", { code: code.trim() }))?.account,
    };
  }

  /**
   * "While you were away" (spec 3.3 §10 K4): the kernel's page after this viewer's cursor,
   * read on in pages while the kernel has more, and marked seen only when the person says so.
   * `read` answers `undefined` when the server keeps no `/proc` ledger.
   */
  whileAway(): WhileAwaySeam {
    return {
      read: async () => {
        let page: AwayPage | undefined;
        try {
          page = await this.optional<AwayPage>(`/kernel/away?viewer=${VIEWER}`);
        } catch (error) {
          // A kernel without an activity ledger says so; the home shows the engine's briefing.
          if (error instanceof ApiError && error.status === 400 && error.code === "kernel.config") return undefined;
          throw error;
        }
        for (let read = 1; page?.more && page.through !== undefined && read < AWAY_PAGES; read++) {
          const next = await this.request<AwayPage>(`/kernel/away?viewer=${VIEWER}&after=${page.through}`);
          // A page held back for a row still being written ends the reading for now.
          if (next.through === undefined || next.through === page.through) break;
          page = mergeAway(page, next);
        }
        return page;
      },
      markSeen: async (through: number) => {
        await this.post("/kernel/away/seen", { viewer: VIEWER, through });
      },
      record: (seq: number) => this.request<AwayRecord>(`/kernel/away/lines/${seq}`),
    };
  }

  /**
   * A kernel route the server may serve without what backs it: `undefined` when it is not
   * mounted, or when the kernel says it was set up without it (`kernel.config`, such as a
   * kernel without long-term memory or without a signed record).
   */
  private async configured<T>(path: string): Promise<T | undefined> {
    try {
      return await this.optional<T>(path);
    } catch (error) {
      if (error instanceof ApiError && error.status === 400 && error.code === "kernel.config") return undefined;
      throw error;
    }
  }

  /**
   * What your agents remember (spec 3.3 §10 K5): the caller's live facts and the latest
   * overnight tidy-up, corrected through the kernel's memory routes, which take the person's
   * answers only (never a paired device's).
   */
  memory(): MemorySeam {
    const fact = (id: string, action: string) => `/kernel/memory/facts/${encodeURIComponent(id)}/${action}`;
    return {
      facts: async () => {
        const read = await this.configured<MemoryResponse>("/kernel/memory");
        return read && memoryOf(read);
      },
      confirm: async (id: string) => {
        await this.post(fact(id, "confirm"), {});
      },
      fix: async (id: string, text: string) => {
        await this.post(fact(id, "fix"), { text });
      },
      forget: async (id: string) => {
        await this.post(fact(id, "forget"), {});
      },
      undo: async (tidyUp: string) => undoOutcome(await this.post<TidyUpUndo>("/kernel/memory/undo", { tidyUp })),
    };
  }

  /**
   * What your agents did (spec 3.3 §10 K5): the kernel's signed record and its check, the one
   * `adk-rust audit verify` makes. `check` answers `undefined` where the kernel keeps no record.
   */
  record(): RecordSeam {
    return {
      check: async () => {
        const check = await this.configured<KernelRecordCheck>("/kernel/record/check");
        if (!check) return undefined;
        const failures = check.report?.failures ?? [];
        // The records after the log's last signed checkpoint are its unsigned tail, which the
        // check passes once any checkpoint verified and fails as `unanchored` before the first.
        const tail = check.report?.unsignedTail ?? 0;
        const unsigned = tail > 0 && failures.every((failure) => failure.kind === "unanchored") ? tail : undefined;
        return {
          intact: check.valid && unsigned === undefined,
          count: check.entries,
          checkedAt: check.checkedAt,
          problem: check.problem,
          unsigned,
        };
      },
      lines: async (limit: number) =>
        (await this.configured<{ lines: KernelRecordLine[] }>(`/kernel/record?limit=${limit}`))?.lines ?? [],
    };
  }

  /**
   * Each agent's spend today in each area (spec 3.3 §12 phase 4), summed over the kernel's
   * processes from the same daily totals as the area's "spent today". Every process is read,
   * as the kernel lists the oldest first and a limit would leave today's out.
   */
  roster(): RosterSeam {
    return {
      spending: async () => {
        const page = await this.optional<{ processes: KernelProcess[] }>("/kernel/processes");
        if (!page) return undefined;
        const spent = new Map<string, AgentSpend>();
        for (const process of page.processes) {
          const area = process.meta.partition ?? "default";
          const key = `${area}\u001f${process.run.target}`;
          const entry = spent.get(key) ?? { agent: process.run.target, area, spentToday: 0 };
          entry.spentToday += process.view?.spendToday ?? 0;
          spent.set(key, entry);
        }
        return [...spent.values()];
      },
    };
  }

  /** Quiet time, the daily spending limit and the mode of each area (spec 3.3 §10 K3). */
  limits(): LimitsSeam {
    const area = (partition: string) => `/kernel/settings/${encodeURIComponent(partition)}`;
    return {
      areas: async () => (await this.optional<{ areas: AreaSettings[] }>("/kernel/settings"))?.areas,
      area: async (partition: string) => (await this.request<{ area: AreaSettings }>(area(partition))).area,
      setQuietTime: async (partition: string, window: QuietWindow | null) =>
        (await this.post<{ area: AreaSettings }>(`${area(partition)}/quiet-time`, { window })).area,
      setSpendingLimit: async (partition: string, microUsd: number | null) =>
        (await this.post<{ area: AreaSettings }>(`${area(partition)}/spending-limit`, { microUsd })).area,
      setMode: async (partition: string, mode: AreaMode) =>
        (await this.post<{ area: AreaSettings }>(`${area(partition)}/mode`, { mode })).area,
    };
  }
}

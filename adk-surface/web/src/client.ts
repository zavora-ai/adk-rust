/**
 * The web home's client: the engine's routes under `/api/surface` (spec 3.3 §6), and the
 * kernel and harness routes beside them. Every read is for the web home (`surface=web`);
 * every command carries the surface and how the input was given (§11).
 */
import type {
  AgentRow,
  AgentsResponse,
  Briefing,
  Command,
  CommandOutcome,
  DecisionCard,
  DecisionsResponse,
  FeedEntry,
  Origin,
  QueuedResponse,
  StatusLine,
  TakeOverRequest,
} from "./generated/api";
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
  QuietWindow,
  StandingRule,
  WakeSet,
} from "./kernel";
import { devicePowers } from "./kernel";
import { labels } from "./labels";
import type { DevicesSeam, LimitsSeam, WhileAwaySeam } from "./seams";
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
  /** Any other JSON route, for the developer panel. */
  get<T>(path: string): Promise<T>;
  /** Sends the server's token from now on, for a server that asks for one. */
  signIn?(token: string): void;
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
 */
export class HttpClient implements HomeClient {
  private token: string | undefined = readStored<string | undefined>(TOKEN_KEY, undefined);

  constructor(private readonly base = "/api") {}

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
    return this.request<StatusLine>(`/surface/status?surface=web${query}`);
  }

  async agents() {
    return (await this.request<AgentsResponse>("/surface/agents?surface=web")).agents;
  }

  async decisions() {
    return (await this.request<DecisionsResponse>("/surface/decisions?surface=web")).decisions;
  }

  decision(id: string) {
    return this.request<DecisionCard>(`/surface/decisions/${encodeURIComponent(id)}?surface=web`);
  }

  briefing(since?: string) {
    const query = since ? `&since=${encodeURIComponent(since)}` : "";
    return this.request<Briefing>(`/surface/briefing?surface=web${query}`);
  }

  command(command: Command, origin: Origin) {
    return this.request<CommandOutcome>("/surface/commands", {
      method: "POST",
      body: JSON.stringify({ surface: "web", origin, command }),
    });
  }

  async *feed(session: string, after: number | undefined, signal: AbortSignal): AsyncIterable<FeedMessage> {
    const headers = this.headers({ accept: "text/event-stream" });
    if (after !== undefined) headers["last-event-id"] = String(after);
    const response = await fetch(
      `${this.base}/surface/sessions/${encodeURIComponent(session)}/feed?surface=web`,
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
      `/surface/sessions/${encodeURIComponent(session)}/queued?surface=web`,
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
          body: JSON.stringify({ surface: "web", version: card.version, secret } satisfies TakeOverRequest),
        }),
    };
  }

  async processes() {
    const page = await this.optional<{ processes: KernelProcess[] }>("/kernel/processes?state=waiting&limit=200");
    return page?.processes;
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

  get<T>(path: string) {
    return this.request<T>(path);
  }

  /**
   * Where you can say yes from (spec 3.3 §10 K1, K2): the kernel's paired devices and chat
   * accounts, removed and added through its pairing routes.
   */
  devices(): DevicesSeam {
    return {
      list: async () => {
        const paired = await this.optional<{ devices: KernelDevice[] }>("/kernel/devices");
        if (!paired) return undefined;
        const channels = await this.optional<{ accounts: ChannelAccount[] }>("/kernel/channels");
        return [
          ...paired.devices
            .filter((device) => device.state === "active")
            .map((device) => ({ id: `device:${device.id}`, name: device.name, may: devicePowers(device.scopes) })),
          ...(channels?.accounts ?? [])
            .filter((account) => account.state === "active")
            .map((account) => ({
              id: `channel:${account.id}`,
              name: account.channel.charAt(0).toUpperCase() + account.channel.slice(1),
              may: labels.yesOnceOrNo,
            })),
        ];
      },
      remove: async (id: string) => {
        const [kind, rest] = [id.slice(0, id.indexOf(":")), id.slice(id.indexOf(":") + 1)];
        const path = kind === "channel" ? "/kernel/channels" : "/kernel/devices";
        await this.request(`${path}/${encodeURIComponent(rest)}/revoke`, { method: "POST", body: "{}" });
      },
      pair: async () => (await this.request<{ code: string }>("/kernel/pairing", { method: "POST", body: "{}" })).code,
      waiting: async () => (await this.optional<{ waiting?: ChannelWaiting[] }>("/kernel/channels"))?.waiting ?? [],
      accept: async (code: string) => {
        await this.post("/kernel/channels/pair", { code: code.trim() });
      },
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

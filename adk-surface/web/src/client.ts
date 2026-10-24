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
import type { AgendaItem, ChannelAccount, KernelDevice, KernelProcess, StandingRule, WakeSet } from "./kernel";
import { devicePowers } from "./kernel";
import { labels } from "./labels";
import type { DevicesSeam } from "./seams";
import { parseSse } from "./sse";
import { readStored, writeStored } from "./stored";

/** Where this tab keeps the server's token, when the server asks for one. */
const TOKEN_KEY = "adk.home.token";

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
  status(): Promise<StatusLine>;
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
  /** The kernel's agenda; `undefined` without one. */
  agenda(): Promise<AgendaItem[] | undefined>;
  answerAgenda(id: string, yes: boolean): Promise<void>;
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
    const value = JSON.parse(text) as { detail?: string; title?: string; message?: string; code?: string };
    return new ApiError(
      response.status,
      value.detail ?? value.message ?? value.title ?? `${response.status} ${response.statusText}`,
      value.code,
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

  status() {
    return this.request<StatusLine>("/surface/status?surface=web");
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
    const page = await this.optional<{ items: AgendaItem[] }>("/kernel/agenda?limit=200");
    return page?.items;
  }

  async answerAgenda(id: string, yes: boolean) {
    await this.request(`/kernel/agenda/${encodeURIComponent(id)}/${yes ? "approve" : "reject"}`, {
      method: "POST",
      body: "{}",
    });
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
    };
  }
}

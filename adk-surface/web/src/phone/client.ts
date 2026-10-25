/**
 * The phone's client (spec 3.3 §5.4, §8.2): the surface routes a paired phone may reach with
 * its device token — reads, its answers, Stop all, "Confirm it's you" — and pairing. A phone
 * never sends a command: commands are closed to devices.
 */
import type {
  AgentRow,
  AgentsResponse,
  Answer,
  Briefing,
  CommandOutcome,
  DecisionCard,
  DecisionsResponse,
  PhoneInfo,
  StatusLine,
} from "../generated/api";
import { ApiError } from "../client";
import { parseSse } from "../sse";
import type { TokenStore } from "./idb";
import { confirmAnswer, registerPasskey } from "./passkey";
import { subscribe as browserSubscribe } from "./push";

/** One frame of a session's feed the decide screen watches. */
export type PhoneFrame = { type: "decisions"; cards: DecisionCard[] } | { type: "other" };

/** What the phone's screens read and do. Tests and previews give their own. */
export interface PhoneClient {
  /** The phone is paired here. */
  paired(): boolean;
  status(): Promise<StatusLine>;
  agents(): Promise<AgentRow[]>;
  briefing(): Promise<Briefing>;
  /** The open decisions, and those decided since `decidedSince`. */
  decisions(decidedSince?: string): Promise<DecisionCard[]>;
  decision(id: string): Promise<DecisionCard>;
  /** Answers through the device's own route; never for an answer that needs presence. */
  answer(card: DecisionCard, answer: Answer, expanded: boolean): Promise<CommandOutcome>;
  /** "Confirm it's you and approve": options, the passkey, then the bound answer. */
  confirm(card: DecisionCard, answer: Answer, expanded: boolean, editedArgs?: unknown): Promise<CommandOutcome>;
  stop(): Promise<CommandOutcome>;
  info(): Promise<PhoneInfo>;
  /** Claims a pairing code and keeps the device token. */
  claim(code: string): Promise<void>;
  /** Asks for notifications and subscribes to wake-ups. */
  allowNotifications(): Promise<boolean>;
  /** Sets up this phone's passkey. */
  addPasskey(): Promise<void>;
  signOut(): Promise<void>;
  /** The session's feed, for the decide screen, until `signal` aborts. */
  watch(session: string, signal: AbortSignal): AsyncIterable<PhoneFrame>;
}

async function problem(response: Response): Promise<ApiError> {
  const text = await response.text();
  try {
    const value = JSON.parse(text) as { error?: { message?: string; code?: string } };
    return new ApiError(response.status, value.error?.message ?? `${response.status}`, value.error?.code);
  } catch {
    return new ApiError(response.status, text || `${response.status} ${response.statusText}`);
  }
}

/** The phone's client over HTTP, relative to the page's own server. */
export class HttpPhoneClient implements PhoneClient {
  private constructor(
    private readonly tokens: TokenStore,
    private token: string | undefined,
    private readonly base: string,
  ) {}

  /** A client with the token `tokens` holds. */
  static async open(tokens: TokenStore, base = "/api"): Promise<HttpPhoneClient> {
    return new HttpPhoneClient(tokens, await tokens.read(), base);
  }

  paired() {
    return this.token !== undefined;
  }

  private async request<T>(path: string, init?: RequestInit): Promise<T> {
    const response = await fetch(`${this.base}${path}`, {
      credentials: "same-origin",
      ...init,
      headers: {
        accept: "application/json",
        ...(init?.body ? { "content-type": "application/json" } : {}),
        ...(this.token ? { authorization: `Bearer ${this.token}` } : {}),
      },
    });
    if (!response.ok) throw await problem(response);
    if (response.status === 204) return undefined as T;
    const text = await response.text();
    return (text ? JSON.parse(text) : undefined) as T;
  }

  private post<T>(path: string, body?: unknown): Promise<T> {
    return this.request<T>(path, { method: "POST", ...(body === undefined ? {} : { body: JSON.stringify(body) }) });
  }

  status() {
    return this.request<StatusLine>("/surface/status?surface=phone");
  }

  async agents() {
    return (await this.request<AgentsResponse>("/surface/agents?surface=phone")).agents;
  }

  briefing() {
    return this.request<Briefing>("/surface/briefing?surface=phone");
  }

  async decisions(decidedSince?: string) {
    const since = decidedSince ? `&decidedSince=${encodeURIComponent(decidedSince)}` : "";
    return (await this.request<DecisionsResponse>(`/surface/decisions?surface=phone${since}`)).decisions;
  }

  decision(id: string) {
    return this.request<DecisionCard>(`/surface/decisions/${encodeURIComponent(id)}?surface=phone`);
  }

  answer(card: DecisionCard, answer: Answer, expanded: boolean) {
    return this.post<CommandOutcome>(`/surface/decisions/${encodeURIComponent(card.id)}/answer`, {
      version: card.version,
      answer,
      expanded,
    });
  }

  confirm(card: DecisionCard, answer: Answer, expanded: boolean, editedArgs?: unknown) {
    return confirmAnswer(this.post.bind(this), card.id, {
      version: card.version,
      answer,
      expanded,
      ...(editedArgs === undefined ? {} : { editedArgs }),
    });
  }

  stop() {
    return this.post<CommandOutcome>("/surface/stop", {});
  }

  info() {
    return this.request<PhoneInfo>("/surface/phone");
  }

  async claim(code: string) {
    const claimed = await this.post<{ token: string }>("/kernel/pairing/claim", { code: code.trim() });
    await this.tokens.write(claimed.token);
    this.token = claimed.token;
  }

  async allowNotifications() {
    const info = await this.info();
    const subscription = await browserSubscribe(info.pushKey);
    if (!subscription?.endpoint || !subscription.keys) return false;
    await this.request("/surface/phone/push", {
      method: "PUT",
      body: JSON.stringify({ endpoint: subscription.endpoint, keys: subscription.keys }),
    });
    return true;
  }

  addPasskey() {
    return registerPasskey(this.post.bind(this));
  }

  async signOut() {
    await this.tokens.clear();
    this.token = undefined;
  }

  async *watch(session: string, signal: AbortSignal): AsyncIterable<PhoneFrame> {
    const response = await fetch(`${this.base}/surface/sessions/${encodeURIComponent(session)}/feed?surface=phone`, {
      headers: { accept: "text/event-stream", ...(this.token ? { authorization: `Bearer ${this.token}` } : {}) },
      signal,
      credentials: "same-origin",
    });
    if (!response.ok) throw await problem(response);
    if (!response.body) return;
    for await (const frame of parseSse(response.body)) {
      if (frame.event === "decisions") {
        yield { type: "decisions", cards: JSON.parse(frame.data) as DecisionCard[] };
      } else {
        yield { type: "other" };
      }
    }
  }
}

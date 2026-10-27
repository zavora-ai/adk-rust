/**
 * Screen actions (spec 3.3 §2 item 7): the catalogue and the person's Coming up under
 * `/api/surface/verbs`, and taking one. A server built without them answers 404, 405 or 501,
 * and the home shows nothing of them (`features.ts`).
 */
import type { Origin } from "../generated/api";
import type { ComingUpItem, SnoozeChoice, VerbCallRequest, VerbDone, VerbsResponse } from "../generated/verbs";
import { ApiError } from "../client";
import { probe } from "../features";
import { readStored } from "../stored";

/** Where the home keeps the server's token. */
const TOKEN_KEY = "adk.home.token";

export interface VerbsClient {
  /** The catalogue and the person's Coming up; `undefined` when the server has no screen actions. */
  list(): Promise<VerbsResponse | undefined>;
  /** Moves `item` to `choice`, answered once it ran. */
  snooze(item: ComingUpItem, choice: SnoozeChoice, origin: Origin): Promise<VerbDone>;
}

async function problem(response: Response): Promise<ApiError> {
  const text = await response.text();
  try {
    const value = JSON.parse(text) as { error?: { message?: string; code?: string } };
    return new ApiError(response.status, value.error?.message ?? text, value.error?.code);
  } catch {
    return new ApiError(response.status, text || `${response.status} ${response.statusText}`);
  }
}

/** The screen actions' routes over HTTP, relative to the page's own server. */
export class HttpVerbsClient implements VerbsClient {
  constructor(private readonly base = "/api/surface/verbs") {}

  private async request<T>(path: string, init?: RequestInit): Promise<T> {
    const token = readStored<string | undefined>(TOKEN_KEY, undefined);
    const response = await fetch(`${this.base}${path}`, {
      credentials: "same-origin",
      ...init,
      headers: {
        accept: "application/json",
        ...(init?.body ? { "content-type": "application/json" } : {}),
        ...(token ? { authorization: `Bearer ${token}` } : {}),
      },
    });
    if (!response.ok) throw await problem(response);
    return (await response.json()) as T;
  }

  list() {
    return probe(() => this.request<VerbsResponse>("?surface=web"));
  }

  snooze(item: ComingUpItem, choice: SnoozeChoice, origin: Origin) {
    const body: VerbCallRequest = {
      surface: "web",
      origin,
      intent: { verb: "snooze", objects: [{ kind: "comingUp", id: item.id }], args: { to: choice.to } },
    };
    return this.request<VerbDone>("/call", { method: "POST", body: JSON.stringify(body) });
  }
}

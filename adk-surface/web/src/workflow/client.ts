/**
 * The workflow observer's client (spec 3.3 §12): the Runs tab's routes under
 * `/api/surface/workflow`, with the home's token. A server built without the `workflow`
 * feature answers 404: `graphs()` then resolves to `undefined` and the tab stays hidden.
 */
import type {
  ForkOutcome,
  ForkRequest,
  GraphView,
  GraphsResponse,
  StepDetail,
  WorkflowChanged,
  WorkflowRun,
} from "../generated/workflow";
import { ApiError } from "../client";
import { probe } from "../features";
import { parseSse } from "../sse";
import { readStored } from "../stored";

/** Where the home keeps the server's token. */
const TOKEN_KEY = "adk.home.token";

export interface WorkflowClient {
  /** The graphs observed; `undefined` when the server serves no workflow routes. */
  graphs(): Promise<GraphView[] | undefined>;
  /** The latest run of `session`, of `graph` when named. */
  run(session: string, graph?: string): Promise<WorkflowRun>;
  /** One step of `session`, read from its checkpoint. */
  step(session: string, checkpoint: string): Promise<StepDetail>;
  fork(session: string, request: ForkRequest): Promise<ForkOutcome>;
  /** Starts a fork made and not started. */
  start(session: string): Promise<ForkOutcome>;
  /** `changed` frames of `session`'s run, until `signal` aborts or the server closes. */
  changes(session: string, signal: AbortSignal): AsyncIterable<WorkflowChanged>;
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

/** The routes over HTTP, relative to the page's own server. */
export class HttpWorkflowClient implements WorkflowClient {
  constructor(private readonly base = "/api/surface/workflow") {}

  private headers(extra: Record<string, string>): Record<string, string> {
    const token = readStored<string | undefined>(TOKEN_KEY, undefined);
    return { ...extra, ...(token ? { authorization: `Bearer ${token}` } : {}) };
  }

  private async request<T>(path: string, init?: RequestInit): Promise<T> {
    const response = await fetch(`${this.base}${path}`, {
      credentials: "same-origin",
      ...init,
      headers: this.headers({
        accept: "application/json",
        ...(init?.body ? { "content-type": "application/json" } : {}),
      }),
    });
    if (!response.ok) throw await problem(response);
    return (await response.json()) as T;
  }

  async graphs() {
    return (await probe(() => this.request<GraphsResponse>("/graphs")))?.graphs;
  }

  run(session: string, graph?: string) {
    const query = graph ? `?graph=${encodeURIComponent(graph)}` : "";
    return this.request<WorkflowRun>(`/sessions/${encodeURIComponent(session)}/run${query}`);
  }

  step(session: string, checkpoint: string) {
    return this.request<StepDetail>(
      `/sessions/${encodeURIComponent(session)}/checkpoints/${encodeURIComponent(checkpoint)}`,
    );
  }

  fork(session: string, request: ForkRequest) {
    return this.request<ForkOutcome>(`/sessions/${encodeURIComponent(session)}/fork`, {
      method: "POST",
      body: JSON.stringify(request),
    });
  }

  start(session: string) {
    return this.request<ForkOutcome>(`/sessions/${encodeURIComponent(session)}/start`, { method: "POST" });
  }

  async *changes(session: string, signal: AbortSignal): AsyncIterable<WorkflowChanged> {
    const response = await fetch(`${this.base}/sessions/${encodeURIComponent(session)}/feed`, {
      credentials: "same-origin",
      headers: this.headers({ accept: "text/event-stream" }),
      signal,
    });
    if (!response.ok) throw await problem(response);
    if (!response.body) return;
    for await (const frame of parseSse(response.body)) {
      if (frame.event === "changed") yield JSON.parse(frame.data) as WorkflowChanged;
    }
  }
}

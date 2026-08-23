import { parseSse } from "./sse";
import type { A2aCard, AgentDetails, RuntimeEvent, Session, TraceSpan, UiCapabilities } from "./types";

async function problem(response: Response): Promise<Error> {
  const text = await response.text();
  try {
    const value = JSON.parse(text) as { detail?: string; title?: string; message?: string };
    return new Error(value.detail ?? value.message ?? value.title ?? `${response.status} ${response.statusText}`);
  } catch {
    return new Error(text || `${response.status} ${response.statusText}`);
  }
}

async function json<T>(response: Response): Promise<T> {
  if (!response.ok) throw await problem(response);
  return response.json() as Promise<T>;
}

export class AdkClient {
  constructor(readonly baseUrl: string) {}

  private url(path: string) {
    return `${this.baseUrl.replace(/\/$/, "")}${path}`;
  }

  private rootUrl(path: string) {
    const base = this.baseUrl.replace(/\/$/, "").replace(/\/api$/, "");
    return `${base}${path}`;
  }

  listAgents() {
    return fetch(this.url("/apps")).then((response) => json<string[]>(response));
  }

  agent(name: string) {
    return fetch(this.url(`/ui/agents/${encodeURIComponent(name)}`)).then((response) => json<AgentDetails>(response));
  }

  capabilities() {
    return fetch(this.url("/ui/capabilities")).then((response) => json<UiCapabilities>(response));
  }

  async a2aCard(): Promise<{ card: A2aCard; path: string } | undefined> {
    for (const path of ["/.well-known/agent.json", "/.well-known/agent-card.json"]) {
      const response = await fetch(this.rootUrl(path));
      if (response.ok) return { card: await response.json() as A2aCard, path };
      if (response.status !== 404) throw await problem(response);
    }
    return undefined;
  }

  createSession(appName: string, userId: string) {
    return fetch(this.url(`/apps/${encodeURIComponent(appName)}/users/${encodeURIComponent(userId)}/sessions`), {
      method: "POST",
      headers: { "content-type": "application/json" },
      body: "{}",
    }).then((response) => json<Session>(response));
  }

  getSession(appName: string, userId: string, sessionId: string) {
    return fetch(this.url(`/apps/${encodeURIComponent(appName)}/users/${encodeURIComponent(userId)}/sessions/${encodeURIComponent(sessionId)}`))
      .then((response) => json<Session>(response));
  }

  listSessions(appName: string, userId: string) {
    return fetch(this.url(`/apps/${encodeURIComponent(appName)}/users/${encodeURIComponent(userId)}/sessions`))
      .then((response) => json<Session[]>(response));
  }

  listArtifacts(appName: string, userId: string, sessionId: string) {
    return fetch(this.url(`/sessions/${encodeURIComponent(appName)}/${encodeURIComponent(userId)}/${encodeURIComponent(sessionId)}/artifacts`))
      .then((response) => json<string[]>(response));
  }

  listTraces(sessionId: string) {
    return fetch(this.url(`/debug/trace/session/${encodeURIComponent(sessionId)}`))
      .then((response) => json<TraceSpan[]>(response));
  }

  async run(
    appName: string,
    userId: string,
    sessionId: string,
    message: string,
    attachments: File[],
    signal: AbortSignal,
    onEvent: (event: RuntimeEvent) => void,
  ) {
    const parts: Array<Record<string, unknown>> = [{ text: message }];
    for (const file of attachments) {
      const bytes = new Uint8Array(await file.arrayBuffer());
      let binary = "";
      for (let offset = 0; offset < bytes.length; offset += 0x8000) {
        binary += String.fromCharCode(...bytes.subarray(offset, offset + 0x8000));
      }
      parts.push({ inlineData: { displayName: file.name, data: btoa(binary), mimeType: file.type || "application/octet-stream" } });
    }
    const response = await fetch(this.url("/run_sse"), {
      method: "POST",
      signal,
      headers: { "content-type": "application/json", "x-adk-ui-protocol": "adk_ui" },
      body: JSON.stringify({
        appName,
        userId,
        sessionId,
        streaming: true,
        newMessage: { role: "user", parts },
      }),
    });
    if (!response.ok) throw await problem(response);
    if (!response.body) throw new Error("The runtime returned no event stream.");
    for await (const frame of parseSse(response.body)) {
      if (!frame.data || frame.data === "[DONE]") continue;
      let parsed: unknown;
      try {
        parsed = JSON.parse(frame.data);
      } catch {
        throw new Error(`The runtime emitted invalid JSON: ${frame.data.slice(0, 120)}`);
      }
      const event = (parsed as { event?: RuntimeEvent }).event ?? (parsed as RuntimeEvent);
      if (event && typeof event === "object") onEvent(event);
    }
  }
}

export async function loadClient(): Promise<AdkClient> {
  try {
    const response = await fetch("/ui/assets/config/runtime-config.json");
    const config = await json<{ backendUrl: string }>(response);
    return new AdkClient(config.backendUrl || "/api");
  } catch {
    return new AdkClient("/api");
  }
}

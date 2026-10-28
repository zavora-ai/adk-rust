/**
 * Apps from MCP servers in a thread (spec 3.3 §6.1 `genui-apps`): what the page asks the home
 * about an app, and the bridge between a framed app and the home. The app only ever asks; the
 * home answers, and a person's own press does what it asked for.
 */
import type { AppAnswer, AppOpen, AppPrompt, AppRequest, CommandOutcome } from "../generated/api";

/** What the page reads and sends for apps; absent where the server was built without `genui-apps`. */
export interface AppsSeam {
  /** The app `call` showed in `session`: framed, blocked, or not shown here; `undefined` without `genui-apps`. */
  open(session: string, call: string): Promise<AppOpen | undefined>;
  /** Relays a request the app made of the home. */
  request(session: string, call: string, request: AppRequest): Promise<AppAnswer>;
  /** Sends what the app suggested, after the person pressed Send it. */
  message(session: string, call: string, message: string): Promise<CommandOutcome>;
}

/** The route of the app `call` showed in `session`. */
export function appPath(session: string, call: string): string {
  return `/surface/genui/sessions/${encodeURIComponent(session)}/apps/${encodeURIComponent(call)}`;
}

/** A JSON-RPC 2.0 message between the page and a framed app. */
export interface JsonRpc {
  jsonrpc: "2.0";
  id?: number | string | null;
  method?: string;
  params?: unknown;
  result?: unknown;
  error?: { code: number; message: string };
}

/** JSON-RPC: the home refused the request. */
const REFUSED = -32_000;

/** What the bridge needs of the page around one framed app. */
export interface BridgeHost {
  /** Posts `message` into the app's frame. */
  post(message: JsonRpc): void;
  /** Asks the home about a request the app made. */
  ask(request: AppRequest): Promise<AppAnswer>;
  /** The app's content is `height` CSS pixels tall. */
  resize(height: number): void;
  /** Shows the person what the app asked for. */
  prompt(prompt: AppPrompt): void;
  /** The light or dark scheme the page is drawn in. */
  theme(): "light" | "dark";
  /** The arguments of the call that showed the app. */
  toolInput: unknown;
  /** Its result, as an MCP `CallToolResult`. */
  toolResult: unknown;
}

function isRpc(data: unknown): data is JsonRpc {
  return typeof data === "object" && data !== null && (data as { jsonrpc?: unknown }).jsonrpc === "2.0";
}

function isObject(value: unknown): value is Record<string, unknown> {
  return typeof value === "object" && value !== null && !Array.isArray(value);
}

/**
 * The bridge of one framed app (spec 3.3 §6.1 `genui-apps`, MCP Apps 2026-01-26). It answers
 * the app's notifications itself — `ui/notifications/initialized` with the call's input and
 * result, `ui/notifications/size-changed` with the frame's height — and relays every request
 * to the home, one at a time: a second request while one is open is refused. Nothing the app
 * sends is ever a person's answer to a decision.
 */
export class AppBridge {
  private busy = false;

  constructor(private readonly host: BridgeHost) {}

  /** Handles one message the app's frame posted. */
  async receive(data: unknown): Promise<void> {
    if (!isRpc(data) || typeof data.method !== "string") return;
    const params = data.params ?? {};
    if (data.id === undefined || data.id === null) {
      this.notified(data.method, params);
      return;
    }
    const id = data.id;
    if (this.busy) {
      this.host.post({ jsonrpc: "2.0", id, error: { code: REFUSED, message: "The home is still answering the app's last request" } });
      return;
    }
    this.busy = true;
    let answer: AppAnswer;
    try {
      answer = await this.host.ask({ method: data.method, params });
    } catch {
      this.host.post({ jsonrpc: "2.0", id, error: { code: REFUSED, message: "The home couldn't answer" } });
      return;
    } finally {
      this.busy = false;
    }
    if (answer.prompt) this.host.prompt(answer.prompt);
    if (answer.error) {
      this.host.post({ jsonrpc: "2.0", id, error: answer.error });
      return;
    }
    let result: unknown = answer.result ?? {};
    // The page knows the scheme it is drawn in; the server does not.
    if (data.method === "ui/initialize" && isObject(result)) {
      const context = isObject(result.hostContext) ? result.hostContext : {};
      result = { ...result, hostContext: { ...context, theme: this.host.theme() } };
    }
    this.host.post({ jsonrpc: "2.0", id, result });
  }

  private notified(method: string, params: unknown) {
    switch (method) {
      case "ui/notifications/initialized":
        this.host.post({ jsonrpc: "2.0", method: "ui/notifications/tool-input", params: { arguments: this.host.toolInput ?? {} } });
        this.host.post({ jsonrpc: "2.0", method: "ui/notifications/tool-result", params: this.host.toolResult ?? {} });
        return;
      case "ui/notifications/size-changed": {
        const height = isObject(params) ? Number(params.height) : Number.NaN;
        if (Number.isFinite(height) && height > 0) this.host.resize(height);
        return;
      }
      default:
        // Logging and anything else the app announces changes nothing here.
        return;
    }
  }
}

/** Whether `host` names this computer. */
function loopback(host: string): boolean {
  return host === "localhost" || host === "[::1]" || host.startsWith("127.") || host.endsWith(".localhost");
}

/** The four numbers of `host` when it is an IPv4 address, as a URL normalises one. */
function ipv4(host: string): number[] | undefined {
  const parts = host.split(".");
  if (parts.length !== 4 || !parts.every((part) => /^\d{1,3}$/.test(part))) return undefined;
  return parts.map(Number);
}

/**
 * Whether `host`, a URL's normalised hostname, is this computer or its network: loopback,
 * unspecified, private, link-local and shared addresses (IPv4, IPv6 and IPv4 inside IPv6), and
 * names only the local network resolves. The server refuses the same links first.
 */
export function onThisNetwork(host: string): boolean {
  const name = host.toLowerCase().replace(/\.$/, "");
  if (name.startsWith("[")) {
    const v6 = name.slice(1, -1);
    const mapped = /^::ffff:([0-9a-f]{1,4}):([0-9a-f]{1,4})$/.exec(v6);
    if (mapped) {
      const high = parseInt(mapped[1], 16);
      const low = parseInt(mapped[2], 16);
      return onThisNetwork(`${high >> 8}.${high & 255}.${low >> 8}.${low & 255}`);
    }
    return v6 === "::" || v6 === "::1" || /^f[cd]/.test(v6) || /^fe[89ab]/.test(v6) || v6.startsWith("::ffff:");
  }
  const v4 = ipv4(name);
  if (v4) {
    const [a, b] = v4;
    return (
      a === 0 ||
      a === 10 ||
      a === 127 ||
      (a === 100 && b >= 64 && b < 128) ||
      (a === 169 && b === 254) ||
      (a === 172 && b >= 16 && b < 32) ||
      (a === 192 && b === 168) ||
      a >= 224
    );
  }
  return (
    !name.includes(".") ||
    [".localhost", ".local", ".internal", ".home.arpa", ".lan", ".ts.net"].some((suffix) => name.endsWith(suffix))
  );
}

/** Whether a link an app asked to open goes somewhere on the web, not this computer or its network. */
export function openable(url: string): boolean {
  try {
    const parsed = new URL(url);
    return (parsed.protocol === "https:" || parsed.protocol === "http:") && !onThisNetwork(parsed.hostname);
  } catch {
    return false;
  }
}

/**
 * Whether the page can frame an app at `frameUrl` on `frameOrigin`: the URL is on that origin,
 * over http or https, and a loopback origin is framed only by a page opened on this computer,
 * since another device cannot reach it.
 */
export function frameable(frameUrl: string, frameOrigin: string, pageHost: string): boolean {
  let url: URL;
  let origin: URL;
  try {
    url = new URL(frameUrl);
    origin = new URL(frameOrigin);
  } catch {
    return false;
  }
  if (url.origin !== origin.origin) return false;
  if (url.protocol !== "http:" && url.protocol !== "https:") return false;
  return !loopback(origin.hostname) || loopback(pageHost);
}

/** The scheme the page is drawn in: the person's choice, else the device's. */
export function pageTheme(): "light" | "dark" {
  const chosen = document.documentElement.dataset.theme;
  if (chosen === "light" || chosen === "dark") return chosen;
  return typeof window.matchMedia === "function" && window.matchMedia("(prefers-color-scheme: dark)").matches ? "dark" : "light";
}

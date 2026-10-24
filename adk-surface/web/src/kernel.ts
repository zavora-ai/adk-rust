/**
 * The kernel and harness routes the web home reads besides the engine's, typed by hand to
 * the fields it uses. Each mirrors a Rust type named in its comment; the page tolerates a
 * missing field, and a route the server does not mount reads as `undefined`.
 */

import { labels } from "./labels";

/** A wake (`adk_kernel::Wake`, camelCase, externally tagged). */
export type Wake =
  | { at: string }
  | { cron: { expr: string; missed?: unknown } }
  | Record<string, unknown>
  | string;

/** `adk_kernel::WakeSet`. */
export interface WakeSet {
  anyOf: Wake[];
  expiresAt?: string;
}

/** `adk_kernel::Process`: the parts the page reads. */
export interface KernelProcess {
  run: {
    runId: string;
    target: string;
    sessionId?: string;
    input?: unknown;
    state: string;
    notBefore?: string;
  };
  meta: {
    pid: string;
    kind?: unknown;
    wake?: WakeSet;
    agendaItem?: string;
    labels?: Record<string, string>;
  };
}

/** `adk_kernel::AgendaItem`: the parts the page reads. */
export interface AgendaItem {
  id: string;
  title: string;
  rationale?: string;
  origin: "user" | "consolidation" | { agent: { pid: string } } | { observation: { id: string } } | string;
  state: string | { scheduled: { wake: WakeSet } } | { running: { pid: string } };
  createdAt: string;
  when?: Wake;
}

/** `adk_harness::StandingRule`: the parts the page reads. */
export interface StandingRule {
  id: string;
  tool: string;
  label: string;
  decision: { kind?: string } | string;
  createdAt: string;
  expiresAt?: string | null;
}

/** The state of an agenda item, without its payload. */
export function agendaState(item: AgendaItem): string {
  return typeof item.state === "string" ? item.state : Object.keys(item.state)[0] ?? "";
}

/** Whether an agenda item is a question from a `/proc` observation (spec 3.2 §7.10). */
export function isQuestion(item: AgendaItem): boolean {
  return typeof item.origin === "object" && item.origin !== null && "observation" in item.origin;
}

/** Whether a rule lets a call go ahead (rather than refusing it). */
export function allows(rule: StandingRule): boolean {
  const kind = typeof rule.decision === "string" ? rule.decision : rule.decision.kind;
  return kind === undefined || kind === "allow";
}

/**
 * The next time a wake fires after `now`, for a time wake or a daily cron (`M H * * *`, in
 * UTC as the kernel reads it); `undefined` for any other wake.
 */
export function nextFire(wake: Wake, now: Date): Date | undefined {
  if (typeof wake !== "object" || wake === null) return undefined;
  if ("at" in wake && typeof wake.at === "string") {
    const at = new Date(wake.at);
    return Number.isNaN(at.getTime()) ? undefined : at;
  }
  if ("cron" in wake && typeof wake.cron === "object" && wake.cron !== null) {
    const expr = (wake.cron as { expr?: unknown }).expr;
    if (typeof expr !== "string") return undefined;
    const fields = expr.trim().split(/\s+/);
    // Six fields carry seconds first.
    const [minute, hour, day, month, weekday] = fields.length === 6 ? fields.slice(1) : fields;
    if (!/^\d+$/.test(minute ?? "") || !/^\d+$/.test(hour ?? "")) return undefined;
    if (day !== "*" || month !== "*" || weekday !== "*") return undefined;
    const next = new Date(Date.UTC(now.getUTCFullYear(), now.getUTCMonth(), now.getUTCDate(), Number(hour), Number(minute)));
    if (next.getTime() <= now.getTime()) next.setUTCDate(next.getUTCDate() + 1);
    return next;
  }
  return undefined;
}

/** When a wake fires on the local day of `now`, past or not: a time wake on that day, or a daily cron. */
export function fireToday(wake: Wake, now: Date): Date | undefined {
  const dayStart = new Date(now.getFullYear(), now.getMonth(), now.getDate());
  const at = nextFire(wake, new Date(dayStart.getTime() - 1));
  if (!at) return undefined;
  return at.getFullYear() === now.getFullYear() && at.getMonth() === now.getMonth() && at.getDate() === now.getDate()
    ? at
    : undefined;
}

/** The soonest time any wake of a set fires after `now`. */
export function nextWake(set: WakeSet | undefined, now: Date): Date | undefined {
  let soonest: Date | undefined;
  for (const wake of set?.anyOf ?? []) {
    const at = nextFire(wake, now);
    if (at && (!soonest || at < soonest)) soonest = at;
  }
  return soonest;
}

/** A daily cron expression, in UTC, for a local `HH:MM`. */
export function dailyCron(hhmm: string, now: Date = new Date()): string {
  const [hours, minutes] = hhmm.split(":").map(Number);
  const local = new Date(now.getFullYear(), now.getMonth(), now.getDate(), hours ?? 0, minutes ?? 0);
  return `${local.getUTCMinutes()} ${local.getUTCHours()} * * *`;
}

/** What a sleeping process will do, in the words its starter gave. */
export function processTitle(process: KernelProcess, agenda: AgendaItem[]): string | undefined {
  const label = process.meta.labels?.title;
  if (label) return label;
  const item = agenda.find((entry) => entry.id === process.meta.agendaItem);
  if (item) return item.title;
  const input = process.run.input;
  if (typeof input === "string") return input;
  if (input && typeof input === "object" && typeof (input as { text?: unknown }).text === "string") {
    return (input as { text: string }).text;
  }
  return undefined;
}

/** What a paired device may do (spec 3.3 §10 K2). */
export type DeviceScope = "decide" | "read" | "stop";

/** A paired device as `GET /kernel/devices` lists it: no token, no hash. */
export interface KernelDevice {
  id: string;
  name: string;
  owner: string;
  scopes: DeviceScope[];
  createdAt: string;
  expiresAt: string;
  revokedAt?: string;
  state: "active" | "expired" | "revoked";
}

/** A paired chat account as `GET /kernel/channels` lists it (spec 3.3 §10 K1). */
export interface ChannelAccount {
  id: string;
  channel: string;
  account: string;
  displayName?: string;
  state: "active" | "revoked";
}

/** What a device may do, in plain words (spec 3.3 §8): "yes or no, and stop all". */
export function devicePowers(scopes: DeviceScope[]): string {
  const decide = scopes.includes("decide");
  const stop = scopes.includes("stop");
  if (decide && stop) return labels.yesOrNoAndStop;
  if (decide) return labels.yesOrNo;
  if (stop) return labels.stopOnly;
  return labels.lookOnly;
}

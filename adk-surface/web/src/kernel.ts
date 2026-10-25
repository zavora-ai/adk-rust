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

/**
 * A chat account as the person tells it from another: the name it shows and its account on the
 * channel, since the name is the account's own choice — "Ana (4242)".
 */
export function accountName(account: { account: string; displayName?: string }): string {
  return account.displayName ? `${account.displayName} (${account.account})` : account.account;
}

/** A chat channel's name as the person knows it: `telegram` is Telegram. */
export function channelName(channel: string): string {
  return channel.charAt(0).toUpperCase() + channel.slice(1);
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

/** A chat account that asked to pair and waits for its code (`ChannelPairingView`, spec 3.3 §10 K1). */
export interface ChannelWaiting {
  id: string;
  channel: string;
  account: string;
  displayName?: string;
  requestedAt: string;
  expiresAt: string;
}

/** What a line of "While you were away" says happened (`adk_kernel::control::api::AwayKind`). */
export type AwayKind = "done" | "failed" | "stopped" | "suggestion" | "question" | "decided" | "stoppedAll" | "resumedAll";

/** One line of "While you were away", from one `/proc` ledger row (`AwayLine`, spec 3.3 §10 K4). */
export interface AwayLine {
  /** The ledger row's sequence number. */
  seq: number;
  at: string;
  partition: string;
  /** The agent's label on the ledger: its acting identity, such as `urn:adk:agent:app/finance`, or `kernel`. */
  agent: string;
  kind: AwayKind | string;
  /** The row's effect, such as `failed` or `approve`. */
  effect?: string;
  /** The line's record: `GET` it for the ledger row. */
  record: string;
}

/** How many lines of each kind a page holds (`AwayCounts`). */
export interface AwayCounts {
  done: number;
  failed: number;
  stopped: number;
  suggestions: number;
  questions: number;
  decided: number;
}

/** `GET /kernel/away` (`AwayResponse`, spec 3.3 §10 K4). */
export interface AwayPage {
  viewer: string;
  /** When this viewer last marked it seen; a new viewer's day. */
  since: string;
  after?: number;
  /** The last ledger row the page covers: what marking it seen marks. */
  through?: number;
  /** Rows after `through` wait for the next page. */
  more: boolean;
  counts: AwayCounts;
  spentMicroUsd: number;
  quietTime: { respected: boolean; breaches?: number[] };
  /** Decisions waiting for the caller now. */
  waiting: number;
  lines: AwayLine[];
}

/** A `/proc` ledger row as the record of a line shows it (`adk_kernel::proc::LedgerEntry`). */
export interface LedgerRow {
  seq: number;
  ts: string;
  partition: string;
  agent: string;
  kind: string;
  effect?: string;
  durationMs?: number;
  meta?: Record<string, unknown>;
}

/** `GET /kernel/away/lines/{seq}` (`AwayLineResponse`): a line's record. */
export interface AwayRecord {
  line?: AwayLine;
  entry: LedgerRow;
}

/** A quiet window (`adk_kernel::QuietHours`): local `HH:MM`, at `utcOffsetMinutes` east of UTC when set. */
export interface QuietWindow {
  start: string;
  end: string;
  utcOffsetMinutes?: number;
}

/** How much an area's agents act alone, as the kernel names it (`PartitionMode`). */
export type AreaMode = "read_only" | "plan" | "ask" | "auto";

/** A setting a person changed while the kernel ran (`adk_kernel::control::Change`). */
export interface SettingChange<T> {
  value: T;
  at: string;
  by: string;
}

/** One area's settings in force (`AreaView`, spec 3.3 §10 K3). */
export interface AreaSettings {
  /** The area: a partition, `default` on a kernel without partitions. */
  partition: string;
  /** The person's quiet time; empty is none. */
  quietTime: QuietWindow[];
  /** Windows a workspace sets, which hold beside the person's. */
  alsoQuiet?: QuietWindow[];
  /** Micro-USD per day; absent is no limit. */
  dailySpendingLimit?: number;
  spentToday: number;
  mode?: AreaMode;
  changed?: {
    quietTime?: SettingChange<QuietWindow | null>;
    dailySpendingLimit?: SettingChange<number | null>;
    mode?: SettingChange<AreaMode>;
  };
}

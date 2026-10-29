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
    /** The area it runs in (`PartitionId`). */
    partition?: string;
    kind?: unknown;
    wake?: WakeSet;
    agendaItem?: string;
    labels?: Record<string, string>;
  };
  /** Live aggregates (`ProcessView`): spend in micro-USD from the kernel's daily totals. */
  view?: {
    spendToday?: number;
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
  /** The acting agents it covers, as a glob of their ids; absent is every agent. */
  agent?: string | null;
  /** A session-scoped rule names its session. */
  scope?: { sessionId?: string };
  /** What `harness.rule.*` listings show: "bash: cargo test …", "write_file: within src/ (this session)". */
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

/** The day names the kernel's `cron` crate reads, by `Date.getUTCDay()` (0 is Sunday). */
const WEEKDAYS = [
  ["SUN", "SUNDAY"],
  ["MON", "MONDAY"],
  ["TUE", "TUES", "TUESDAY"],
  ["WED", "WEDNESDAY"],
  ["THU", "THURS", "THURSDAY"],
  ["FRI", "FRIDAY"],
  ["SAT", "SATURDAY"],
];

/**
 * The days of the week a cron weekday field names, as the kernel reads it (`adk_core::schedule`):
 * a five-field expression counts 0 and 7 as Sunday, as Unix cron does, so `1-5` runs Monday to
 * Friday; with `crateNumbering`, for six or seven fields, the `cron` crate counts 1 as Sunday and
 * 7 as Saturday, so `1-5` runs Sunday to Thursday. Names (`MON-FRI`) mean the same days in both.
 * The field is `*`, a day, a range or a list of them; the set holds `Date.getUTCDay()` numbers
 * (0 is Sunday); `undefined` for a field it cannot read or one the kernel refuses.
 */
export function cronWeekdays(field: string, crateNumbering = false): Set<number> | undefined {
  if (field === "*" || field === "?") return new Set([0, 1, 2, 3, 4, 5, 6]);
  const day = (text: string) => {
    const named = WEEKDAYS.findIndex((names) => names.includes(text.toUpperCase()));
    if (named >= 0) return named;
    if (crateNumbering) return /^[1-7]$/.test(text) ? Number(text) - 1 : undefined;
    return /^[0-7]$/.test(text) ? Number(text) : undefined;
  };
  const days = new Set<number>();
  for (const part of field.split(",")) {
    const [from, to, extra] = part.split("-");
    const start = day(from ?? "");
    const end = to === undefined ? start : day(to);
    if (start === undefined || end === undefined || extra !== undefined || end < start) return undefined;
    for (let value = start; value <= end; value++) days.add(value % 7);
  }
  return days;
}

/**
 * The next time a wake fires after `now`, for a time wake or a cron that runs at one time of
 * day on every day or on some days of the week (`M H * * *`, `M H * * MON-FRI`, in UTC as the
 * kernel reads it, as `cronWeekdays` says), so a weekday routine on a Saturday names Monday;
 * `undefined` for any other wake.
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
    if (day !== "*" || month !== "*") return undefined;
    const days = cronWeekdays(weekday ?? "*", fields.length === 6);
    if (!days || days.size === 0) return undefined;
    for (let ahead = 0; ahead <= 7; ahead++) {
      const next = new Date(
        Date.UTC(now.getUTCFullYear(), now.getUTCMonth(), now.getUTCDate() + ahead, Number(hour), Number(minute)),
      );
      if (next.getTime() > now.getTime() && days.has(next.getUTCDay())) return next;
    }
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

/**
 * A quiet window (`adk_kernel::QuietHours`): local `HH:MM` in the IANA `timeZone` when set,
 * following its daylight saving time; else at `utcOffsetMinutes` east of UTC when set; else in
 * the server's time zone.
 */
export interface QuietWindow {
  start: string;
  end: string;
  utcOffsetMinutes?: number;
  timeZone?: string;
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

/** Where a remembered fact came from (`adk_kernel::control::FactOrigin`, spec 3.3 §8.1). */
export type FactOrigin = "youSaidThis" | "youConfirmedThis" | "guess";

/** A fact the agents remember (`RememberedFact`, spec 3.3 §10 K5). */
export interface RememberedFact {
  id: string;
  /** Escaped on display, as every string from a model or a tool is (S-I9). */
  text: string;
  origin: FactOrigin | string;
  kind?: string;
  /** The area whose agents remember it. */
  area?: string;
  /** What noticed a guess: an agent, or an area. */
  noticedBy?: string;
  learnedAt: string;
  /** The tidy-up that wrote it. */
  tidyUp?: string;
}

/** An overnight tidy-up of the memory (`adk_kernel::control::TidyUp`). */
export interface KernelTidyUp {
  id: string;
  area: string;
  at: string;
  finishedAt?: string;
  undoneAt?: string;
  merged: number;
  superseded: number;
  forgotten: number;
  distilled: number;
}

/** `GET /kernel/memory` (`MemoryResponse`). */
export interface MemoryResponse {
  facts: RememberedFact[];
  tidyUps: KernelTidyUp[];
}

/** What undoing a tidy-up did (`TidyUpUndo`): the ids of the facts it restored, forgot and kept. */
export interface TidyUpUndo {
  id: string;
  restored: string[];
  forgotten: string[];
  kept: string[];
}

/** Whether "Undo" applies to a tidy-up (`TidyUp::undoable`): finished, changed something, not undone. */
export function undoable(tidyUp: KernelTidyUp): boolean {
  return (
    tidyUp.finishedAt !== undefined &&
    tidyUp.finishedAt !== null &&
    (tidyUp.undoneAt === undefined || tidyUp.undoneAt === null) &&
    tidyUp.merged + tidyUp.superseded + tidyUp.forgotten + tidyUp.distilled > 0
  );
}

/** One action on the kernel's signed record (`adk_kernel::control::RecordLine`, spec 3.3 §10 K5). */
export interface KernelRecordLine {
  seq: number;
  at: string;
  /** The record's action: `surface.decide`, `kernel.control`, `harness.rule.created`, a tool's name. */
  action: string;
  /** What a kernel record did: `memory.forget`, `settings.mode`, `freeze`. */
  kernelAction?: string;
  subject?: string;
  agent?: string;
  tool?: string;
  /** `approved`, `rejected`, `denied`, `allowed`. */
  decision?: string;
  /** `succeeded`, `failed`, `decided`. */
  outcome?: string;
  /** `keyboard`, `pointer`, `touch`, `voice`, `chat`. */
  how?: string;
  /** The chat channel or device. */
  channel?: string;
  /** The surface it was given on: `web`, `tray`, `phone`, `terminal`, `voice`, `chat`, `device`. */
  surface?: string;
  target?: string;
  /** The item it acted on, as the person knows it: "News digest" (spec 3.3 §8). */
  title?: string;
  /** Who publishes the tool, for a call: `verbs:adk-rust` for a screen action (spec 3.3 §7.1). */
  toolSource?: string;
  /** What decided a call: `confirmation` for a person's answer, `tool_policy:<name>`, `governance`. */
  policyRef?: string;
}

/** `GET /kernel/record/check` (`RecordCheck`): the check `adk-rust audit verify` makes. */
export interface KernelRecordCheck {
  valid: boolean;
  entries: number;
  checkedAt: string;
  problem?: string;
  /** The verifier's report, as `adk-rust audit verify --json` prints it. */
  report?: {
    /** Records no signed checkpoint covers yet. */
    unsignedTail?: number;
    failures?: Array<{ kind?: string; message?: string }>;
  };
}

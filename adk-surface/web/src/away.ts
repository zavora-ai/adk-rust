/**
 * "While you were away" over the kernel's `/proc` ledger (spec 3.3 §5.1 Today, §8.1, §10 K4):
 * one line per event in the words every surface uses, when quiet time last ended, and whether
 * the morning briefing is due. Today and the briefing read the same page, so they never
 * disagree (§12 phase 2).
 */
import type { AgentRow, StatusLine } from "./generated/api";
import type { PillKind } from "./components/Pill";
import type { AgendaItem, AreaSettings, AwayLine, AwayPage, QuietWindow } from "./kernel";
import { agendaState, isQuestion } from "./kernel";
import { labels, words } from "./labels";

/** The ledger's label for the kernel's own work, which names no agent. */
const KERNEL_AGENT = "kernel";

/** Two pages of the same viewer, the second read after the first's `through`, as one. */
export function mergeAway(first: AwayPage, next: AwayPage): AwayPage {
  const counts = { ...first.counts };
  for (const key of Object.keys(counts) as Array<keyof typeof counts>) counts[key] += next.counts[key] ?? 0;
  return {
    ...first,
    through: next.through ?? first.through,
    more: next.more,
    counts,
    spentMicroUsd: first.spentMicroUsd + next.spentMicroUsd,
    quietTime: {
      respected: first.quietTime.respected && next.quietTime.respected,
      breaches: [...(first.quietTime.breaches ?? []), ...(next.quietTime.breaches ?? [])],
    },
    waiting: next.waiting,
    lines: [...first.lines, ...next.lines],
  };
}

/** A name as the engine words one it has no display name for: `finance_bot` → "Finance bot". */
export function humanize(name: string): string {
  const parts: string[] = [];
  let current = "";
  let previousLower = false;
  for (const c of name) {
    if (c === "_" || c === "-" || c === " " || c === ".") {
      if (current) parts.push(current);
      current = "";
      previousLower = false;
      continue;
    }
    const upper = c !== c.toLowerCase();
    if (upper && previousLower && current) {
      parts.push(current);
      current = "";
    }
    previousLower = c !== c.toUpperCase() || /\d/.test(c);
    current += c.toLowerCase();
  }
  if (current) parts.push(current);
  const text = parts.join(" ");
  return text.charAt(0).toUpperCase() + text.slice(1);
}

/**
 * The agent a ledger label names, as the roster shows it: `urn:adk:agent:home/finance` and
 * `finance` are the roster's Finance; `undefined` for the kernel's own work.
 */
export function agentOf(label: string, agents: AgentRow[]): string | undefined {
  if (!label || label === KERNEL_AGENT) return undefined;
  const name = label.split(/[/:]/).pop() || label;
  const row = agents.find(
    (candidate) =>
      candidate.agent.name === label ||
      candidate.agent.name === name ||
      candidate.agent.displayName.toLowerCase() === name.toLowerCase(),
  );
  return row?.agent.displayName ?? humanize(name);
}

/** What a line shows: its pill and its sentence (spec 3.3 §8). */
export interface LineWords {
  pill?: PillKind;
  pillText?: string;
  text: string;
}

/**
 * The words of one line. The ledger is content-free (spec 3.2 §7.10), so a line says what kind
 * of thing happened and links to its record; it never guesses at content. The sentences are
 * the engine's (`adk_surface::words::briefing_sentence`).
 */
export function lineWords(line: AwayLine, agents: AgentRow[]): LineWords {
  const agent = agentOf(line.agent, agents);
  const who = agent ?? words.YOUR_AGENTS;
  switch (line.kind) {
    case "done":
      return { pill: "done", pillText: words.DONE, text: `${who} finished.` };
    case "failed":
      // A process that failed carries its state; the loop guard's row carries none.
      return line.effect === "failed"
        ? { pill: "failed", pillText: words.FAILED, text: `${who} failed.` }
        : { pill: "failed", pillText: words.FAILED, text: `${who} stopped because it was repeating itself.` };
    case "stopped":
      return { pill: "neutral", pillText: labels.stoppedPill, text: `${who} was stopped.` };
    case "suggestion":
      return {
        pill: "suggestion",
        pillText: words.SUGGESTION,
        text: agent ? `${agent} suggests something. Nothing happens until you say so.` : words.AGENTS_SUGGEST,
      };
    case "question":
      return { pill: "question", pillText: words.QUESTION, text: words.NOTICED_SOMETHING };
    case "decided":
      return {
        pill: "decided",
        pillText: words.YOU_DECIDED,
        text:
          line.effect === "deny"
            ? `You said no to ${agent ?? "your agents"}.`
            : `You allowed ${agent ?? "your agents"} to go ahead.`,
      };
    case "stoppedAll":
      return { pill: "neutral", pillText: labels.stoppedPill, text: `${words.ALL_AGENTS_STOPPED}.` };
    case "resumedAll":
      return { pill: "neutral", pillText: labels.resumedPill, text: words.ALL_AGENTS_RESUMED };
    default:
      return { text: `${who} did something.` };
  }
}

/**
 * A suggestion's or question's sentence from its agenda item (spec 3.3 §5.1, §8 "Name the
 * outcome"): what it is about, then what happens now — nothing until the person says so while
 * it is open, else what became of it.
 */
export function itemSentence(item: AgendaItem): string {
  const question = isQuestion(item);
  const state = agendaState(item);
  let outcome: string;
  switch (state) {
    case "proposed":
      outcome = question ? words.IS_THAT_RIGHT : labels.nothingHappensUntilYouSay;
      break;
    case "approved":
    case "scheduled":
      outcome = question ? labels.answeredYes : labels.willBeDone;
      break;
    case "running":
      outcome = question ? labels.answeredYes : labels.workingOnIt;
      break;
    case "done":
      outcome = question ? labels.answeredYes : labels.itIsDone;
      break;
    case "rejected":
      outcome = question ? labels.answeredNo : labels.wontBeDone;
      break;
    case "expired":
      outcome = labels.endedWithoutAnswer;
      break;
    default:
      return item.title;
  }
  return `${item.title} ${outcome}`;
}

/**
 * The lines in the order Today shows them: what failed first, as it needs the person, then
 * everything else as it happened (spec 3.3 §5.1).
 */
export function inOrder<T extends { at: Date; failed: boolean }>(rows: T[]): T[] {
  return [...rows].sort((a, b) => Number(b.failed) - Number(a.failed) || a.at.getTime() - b.at.getTime());
}

// ── Quiet time ────────────────────────────────────────────────────────────

/**
 * Every quiet window in force, as this device's clock reads it: each area's own and the ones its
 * workspace keeps beside them, or the engine's quiet hours where the kernel serves no settings.
 */
export function quietWindows(areas: AreaSettings[] | undefined, status: StatusLine | undefined): QuietWindow[] {
  const host = status?.hostUtcOffsetMinutes ?? undefined;
  const windows = areas
    ? areas.flatMap((area) => [...area.quietTime, ...(area.alsoQuiet ?? [])])
    : status?.quiet
      ? [{ start: status.quiet.start, end: status.quiet.end }]
      : [];
  return windows.map((window) => onThisDevice(window, host));
}

function minutes(hhmm: string): number {
  const [hours, mins] = hhmm.split(":").map(Number);
  return (hours ?? 0) * 60 + (mins ?? 0);
}

/** `HH:MM` of a minute of the day, wrapped into one day. */
function hhmmOf(minute: number): string {
  const day = ((minute % 1440) + 1440) % 1440;
  return `${String(Math.floor(day / 60)).padStart(2, "0")}:${String(day % 60).padStart(2, "0")}`;
}

/** Minutes east of UTC of this device's time zone at `now`. */
export function deviceOffset(now: Date = new Date()): number {
  return -now.getTimezoneOffset();
}

/**
 * A quiet window as this device's clock reads it (spec 3.3 §5.1). The kernel reads a window
 * without an offset of its own in the server's time zone (`host`, the status line's
 * `hostUtcOffsetMinutes`), so its times move from the offset it is kept at to this device's
 * (`here`): 22:00–07:00 kept in UTC reads 01:00–10:00 in Nairobi. Without `host`, a window
 * without an offset is read here as it is.
 */
export function onThisDevice(window: QuietWindow, host: number | undefined, here: number = deviceOffset()): QuietWindow {
  const offset = window.utcOffsetMinutes ?? host;
  if (offset === undefined) return { start: window.start, end: window.end };
  const shift = here - offset;
  return {
    start: shift === 0 ? window.start : hhmmOf(minutes(window.start) + shift),
    end: shift === 0 ? window.end : hhmmOf(minutes(window.end) + shift),
    utcOffsetMinutes: here,
  };
}

/** The wall clock at `now` where the window's times are read: its offset, else this device's zone. */
function wall(now: Date, offset?: number) {
  if (offset === undefined) {
    return { y: now.getFullYear(), m: now.getMonth(), d: now.getDate(), minute: now.getHours() * 60 + now.getMinutes() };
  }
  const shifted = new Date(now.getTime() + offset * 60_000);
  return {
    y: shifted.getUTCFullYear(),
    m: shifted.getUTCMonth(),
    d: shifted.getUTCDate(),
    minute: shifted.getUTCHours() * 60 + shifted.getUTCMinutes(),
  };
}

/** The instant of local minute `minute` on the day `day` days after `{y, m, d}`. */
function instant(clock: { y: number; m: number; d: number }, day: number, minute: number, offset?: number): Date {
  if (offset === undefined) return new Date(clock.y, clock.m, clock.d + day, 0, minute);
  return new Date(Date.UTC(clock.y, clock.m, clock.d + day, 0, minute) - offset * 60_000);
}

/** Whether `now` falls inside `window`, which may cross midnight; equal times are no window. */
export function insideQuiet(window: QuietWindow, now: Date): boolean {
  const start = minutes(window.start);
  const end = minutes(window.end);
  if (start === end) return false;
  const at = wall(now, window.utcOffsetMinutes).minute;
  return start < end ? at >= start && at < end : at >= start || at < end;
}

/** The last time `window` ended at or before `now`. */
function lastEnd(window: QuietWindow, now: Date): Date {
  const clock = wall(now, window.utcOffsetMinutes);
  const end = minutes(window.end);
  return instant(clock, clock.minute >= end ? 0 : -1, end, window.utcOffsetMinutes);
}

/** When `window` ends on the local day of `now`, past or not. */
export function endToday(window: QuietWindow, now: Date): Date {
  const clock = wall(now, window.utcOffsetMinutes);
  return instant(clock, 0, minutes(window.end), window.utcOffsetMinutes);
}

/**
 * When quiet time last ended: the latest end of any window at or before `now`, or `undefined`
 * while any window holds or when there is no quiet time.
 */
export function quietEnded(windows: QuietWindow[], now: Date): Date | undefined {
  const real = windows.filter((window) => minutes(window.start) !== minutes(window.end));
  if (real.length === 0 || real.some((window) => insideQuiet(window, now))) return undefined;
  return real.map((window) => lastEnd(window, now)).reduce((a, b) => (a > b ? a : b));
}

// ── The morning briefing ──────────────────────────────────────────────────

/** Where this browser keeps the quiet time whose briefing the person has seen. */
const BRIEFING_KEY = "adk.home.briefing";

/**
 * The browser's storage that outlives the tab, else the tab's own; `undefined` when storage is
 * off, in a private window or with site data blocked.
 */
function storage(): Storage | undefined {
  for (const pick of [() => window.localStorage, () => window.sessionStorage]) {
    try {
      const store = pick();
      if (typeof store?.getItem === "function" && typeof store.setItem === "function") return store;
    } catch {
      // Blocked: try the next.
    }
  }
  return undefined;
}

/** The end of quiet time whose briefing this browser already showed and the person put away. */
export function briefingShown(): string | undefined {
  try {
    return storage()?.getItem(BRIEFING_KEY) ?? undefined;
  } catch {
    return undefined;
  }
}

/** Remembers that the briefing for the quiet time ending at `end` was seen. */
export function rememberBriefing(end: Date) {
  try {
    storage()?.setItem(BRIEFING_KEY, end.toISOString());
  } catch {
    // Storage is off: the briefing stays until it is marked seen or quiet time starts again.
  }
}

/**
 * The end of quiet time the morning briefing is for, when it is due (spec 3.3 §5.1, §12 phase 2):
 * quiet time has ended since the person last marked "While you were away" seen, something
 * happened or waits, and this browser has not shown it and had it put away already. One quiet
 * time makes one briefing.
 */
export function briefingDue(
  page: AwayPage | undefined,
  windows: QuietWindow[],
  now: Date,
  shown: string | undefined,
): Date | undefined {
  if (!page) return undefined;
  const end = quietEnded(windows, now);
  if (!end || new Date(page.since) >= end) return undefined;
  if (page.lines.length === 0 && page.waiting === 0) return undefined;
  if (shown && new Date(shown).getTime() >= end.getTime()) return undefined;
  return end;
}

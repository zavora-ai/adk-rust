/**
 * "Allowed without asking" (spec 3.3 §5.1, §8.1): each standing rule as a sentence, its end
 * date, and the dates "Extend" may move it to.
 */
import { agentOf } from "./away";
import type { AgentRow } from "./generated/api";
import type { StandingRule } from "./kernel";

const DAY = 24 * 3_600_000;

/** How long "Extend" offers to add by default. */
export const EXTEND_DAYS = 30;

/**
 * What a rule lets agents do, from its label ("bash: cargo test …", "write_file: within src/
 * (this session)", "file writes: within notes/"): "Your agents can run commands that start
 * with cargo test." A rule of one conversation says so.
 */
export function ruleSentence(rule: StandingRule, agents: AgentRow[]): string {
  // A glob covers more than one agent, so it reads as all of them.
  const who = (rule.agent && !/[*?]/.test(rule.agent) ? agentOf(rule.agent, agents) : undefined) ?? "Your agents";
  const [tool, ...rest] = rule.label.split(": ");
  let detail = rest.join(": ").trim();
  const session = / \(this session\)$/.test(detail) || !!rule.scope?.sessionId;
  detail = detail.replace(/ \(this session\)$/, "");
  const name = tool && tool === rule.tool ? tool : rule.tool;
  let what: string;
  // "Always allow for this folder": every file-writing tool, so no one tool's name (spec 3.3 §8.2).
  if (tool === "file writes" && rule.tool === "*") what = `write files ${detail}`;
  else if (name === "bash" && detail) what = `run commands that start with ${detail.replace(/\s*…$/, "")}`;
  else if (detail.startsWith("any page on ")) what = `use ${name} on any page of ${detail.slice("any page on ".length)}`;
  else if (detail.startsWith("searches restricted to ")) what = `search ${detail.slice("searches restricted to ".length)}`;
  else if (detail.startsWith("any arguments from ")) what = `use ${name} with any arguments`;
  else if (detail) what = `use ${name} ${detail}`;
  else what = `use ${name}`;
  return `${who} can ${what}${session ? ", in one conversation" : ""}.`;
}

/** Whether a rule has ended by `now`. */
export function ended(rule: StandingRule, now: Date): boolean {
  return !!rule.expiresAt && new Date(rule.expiresAt).getTime() <= now.getTime();
}

/** The earliest day "Extend" may move a rule to: the day after its end, or after today once it has ended. */
export function earliestExtension(rule: StandingRule, now: Date): Date {
  const end = rule.expiresAt ? new Date(rule.expiresAt) : now;
  const from = end.getTime() > now.getTime() ? end : now;
  return new Date(from.getTime() + DAY);
}

/** The day "Extend" offers first: thirty days after the rule's end, or after today once it has ended. */
export function suggestedExtension(rule: StandingRule, now: Date): Date {
  const end = rule.expiresAt ? new Date(rule.expiresAt) : now;
  const from = end.getTime() > now.getTime() ? end : now;
  return new Date(from.getTime() + EXTEND_DAYS * DAY);
}

/** A date as an `<input type="date">` value, on this device's calendar. */
export function dateValue(at: Date): string {
  const pad = (n: number) => String(n).padStart(2, "0");
  return `${at.getFullYear()}-${pad(at.getMonth() + 1)}-${pad(at.getDate())}`;
}

/** The end of the day a date field names, on this device's clock: a rule "until 30 November" holds all that day. */
export function endOfDay(value: string): Date | undefined {
  const match = /^(\d{4})-(\d{2})-(\d{2})$/.exec(value);
  if (!match) return undefined;
  const [, year, month, day] = match;
  const at = new Date(Number(year), Number(month) - 1, Number(day), 23, 59, 0);
  return Number.isNaN(at.getTime()) ? undefined : at;
}

/**
 * "What your agents did" (spec 3.3 §5.1, §8.1, §11): the latest actions on the kernel's signed
 * record as sentences — "You said yes to send_email, from Telegram." The record carries
 * structured fields (§10 K5), never what a call said, so a sentence names the tool, the answer
 * and where it was given; actions a person never needs to read (housekeeping, messages sent)
 * are left out of the list, though the check counts them.
 */
import { agentOf } from "./away";
import { dayMonth, hhmm } from "./format";
import type { AgentRow } from "./generated/api";
import type { KernelRecordLine } from "./kernel";
import { channelName } from "./kernel";
import type { RecordLine } from "./seams";

/** How many record lines are read to find the latest sentences. */
export const RECORD_READ = 200;

/** How many sentences "What your agents did" lists. */
export const RECORD_SHOWN = 5;

/** Where an answer was given, from how it was given (§11): ", from Telegram". */
function where(line: KernelRecordLine): string {
  switch (line.how) {
    case "pointer":
      return ", in the home";
    case "keyboard":
      return ", from the terminal";
    case "touch":
      return line.channel ? `, from ${line.channel}` : ", from your phone";
    case "voice":
      return ", by voice";
    case "chat":
      return `, from ${channelName(line.channel ?? "chat")}`;
    default:
      return line.channel ? `, from ${line.channel}` : "";
  }
}

/** What a call was, by its tool, or "a call". */
function call(line: KernelRecordLine): string {
  return line.tool ?? "a call";
}

/** The sentence of an answer to a decision: yes or no to the call, and where. */
function answer(line: KernelRecordLine): string | undefined {
  switch (line.decision) {
    case "approved":
    case "edited":
      return `You said yes to ${call(line)}${where(line)}.`;
    case "rejected":
      return `You said no to ${call(line)}${where(line)}.`;
    default:
      return undefined;
  }
}

/** What the kernel's own record of a person's action did (`kernelAction`, spec 3.3 §10 K1–K5). */
function kernelSentence(line: KernelRecordLine): string | undefined {
  switch (line.kernelAction) {
    case "approvals.decide":
    case "decide":
      return answer(line);
    case "memory.confirm":
      return "You confirmed a note your agents keep.";
    case "memory.fix":
      return "You fixed a note your agents keep.";
    case "memory.forget":
      return "You made your agents forget a note.";
    case "memory.undo":
      return "You undid a tidy-up of your agents' notes.";
    case "settings.quiet_time":
      return "You changed quiet time.";
    case "settings.spending_limit":
      return "You changed the daily spending limit.";
    case "settings.mode":
    case "partition.mode":
      return "You changed how much your agents ask first.";
    case "pairing.issue":
      return "You started adding a phone or app.";
    case "pairing.claim":
      return "A phone or app was paired.";
    case "devices.revoke":
      return "You removed a phone or app.";
    case "channels.pair":
      return "You paired a chat account.";
    case "channels.revoke":
      return "You removed a chat account.";
    case "freeze":
      return "You stopped all agents.";
    case "stop":
      return `You stopped all agents${where(line)}.`;
    case "unfreeze":
      return "You resumed all agents.";
    case "agenda.approve":
      return "You said yes to a suggestion.";
    case "agenda.reject":
      return "You said no to a suggestion.";
    default:
      return undefined;
  }
}

/**
 * One action on record as a sentence, or `undefined` for one the list leaves out: process
 * housekeeping, a message sent, the approval service's own copy of an answer a surface records.
 */
export function recordSentence(line: KernelRecordLine, agents: AgentRow[]): string | undefined {
  switch (line.action) {
    case "surface.decide":
      return line.outcome === "decided" ? answer(line) : undefined;
    case "surface.stop_all":
      return `You stopped all agents${where(line)}.`;
    case "surface.resume_all":
      return `You resumed all agents${where(line)}.`;
    case "surface.set_mode":
      return "You changed how much your agents ask first.";
    case "harness.rule.created":
      return `You allowed ${call(line)} without asking.`;
    case "harness.rule.revoked":
      return `You stopped allowing ${call(line)} without asking.`;
    case "harness.rule.extended":
      return `You extended how long ${call(line)} is allowed without asking.`;
    default:
      break;
  }
  if (line.action.startsWith("kernel.")) return kernelSentence(line);
  // A tool call: its action is the tool, and it names the agent that made it.
  if (line.tool !== undefined && line.action === line.tool && line.agent) {
    const agent = agentOf(line.agent, agents) ?? "Your agents";
    if (line.decision === "denied") return `${agent} was not allowed to use ${line.tool}.`;
    if (line.outcome === "failed") return `${agent} tried ${line.tool}, and it failed.`;
    if (line.outcome === "succeeded" || line.decision === "allowed") return `${agent} used ${line.tool}.`;
  }
  return undefined;
}

/** "07:31" within the last day, else "23 Oct". */
function atText(at: Date, now: Date): string {
  return now.getTime() - at.getTime() < 24 * 3_600_000 ? hhmm(at) : dayMonth(at).replace(/(\d+ \w{3})\w*/, "$1");
}

/** The latest actions on record as sentences, newest first, at most `shown`. */
export function recentLines(
  lines: KernelRecordLine[],
  agents: AgentRow[],
  now: Date = new Date(),
  shown: number = RECORD_SHOWN,
): RecordLine[] {
  const out: RecordLine[] = [];
  for (const line of lines) {
    const text = recordSentence(line, agents);
    if (!text) continue;
    const at = new Date(line.at);
    out.push({ seq: line.seq, at: line.at, atText: atText(at, now), text });
    if (out.length >= shown) break;
  }
  return out;
}

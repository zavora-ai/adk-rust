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

/** Where an answer was given, from the surface and how it was given (§11): ", from Telegram". */
function where(line: KernelRecordLine): string {
  if (line.surface === "tray") return ", from the tray";
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

/** Where a screen action was taken (spec 3.3 §2 item 7): "in the terminal", "by voice". */
function screenWhere(line: KernelRecordLine): string {
  switch (line.surface) {
    case "terminal":
      return " in the terminal";
    case "web":
      return " in the home app";
    case "voice":
      return " by voice";
    case "phone":
      return " on your phone";
    case "tray":
      return " from the tray";
    case "chat":
      return line.channel ? ` in ${channelName(line.channel)}` : " in a chat";
    default:
      return "";
  }
}

/**
 * A screen action's sentence (spec 3.3 §2 item 7): "You snoozed News digest by voice.", and an
 * agent's request "Finance asked to snooze News digest; you said yes on your phone." The record
 * names the item by its title; a kernel id is never shown (§8). It does not say why a request
 * was refused, so the sentence does not either.
 */
function screenAction(line: KernelRecordLine, agents: AgentRow[]): string | undefined {
  if (line.tool !== "snooze") return undefined;
  const what = line.target && !line.target.startsWith("agenda:") ? line.target : "something in Coming up";
  const agent = line.agent ? agentOf(line.agent, agents) : undefined;
  if (!agent) return line.outcome === "done" ? `You snoozed ${what}${screenWhere(line)}.` : undefined;
  if (line.outcome === "done") return `${agent} asked to snooze ${what}; you said yes${screenWhere(line)}.`;
  return `${agent} asked to snooze ${what}; it couldn't be done, so nothing changed.`;
}

/** The agent a person's own screen actions run under: `urn:adk:agent:<app>/surface.verbs`. */
const VERBS_AGENT = "/surface.verbs";

/**
 * The governed path's record of a call to a screen action, told by its publisher (spec 3.3
 * §2 item 7, §7.1, §8). The action's own records say what happened, by the item's title — "You
 * snoozed News digest", a person's yes or no to an agent's request — so this one adds a line
 * only for an agent's request refused before anyone answered it, and never names the tool.
 */
function screenCall(line: KernelRecordLine, agents: AgentRow[]): string | undefined {
  const refused = line.decision === "denied" && line.policyRef !== "confirmation";
  if (!refused || line.tool !== "verb_snooze" || !line.agent || line.agent.endsWith(VERBS_AGENT)) return undefined;
  return `${agentOf(line.agent, agents) ?? "Your agent"} asked to snooze something in Coming up; it couldn't be done, so nothing changed.`;
}

/**
 * A decision the kernel itself asks, in plain words, by what it is about — never its tool,
 * `kernel:agent.create` (spec 3.3 §8): "adding Finance", "News digest", "starting email for
 * your agents"; a record that names nothing says what kind of decision it was.
 */
function kernelDecision(line: KernelRecordLine): string | undefined {
  if (!line.tool?.startsWith("kernel:")) return undefined;
  const name = line.title;
  switch (line.tool) {
    case "kernel:agent.create":
      return `adding ${name ?? "an agent"}`;
    case "kernel:agenda":
      return name ?? "a suggestion";
    case "kernel:driver.mount":
      return `starting ${name ?? "a service"} for your agents`;
    default:
      return name ?? "a request from ADK-Rust";
  }
}

/** What a call was, by its tool, or "a call"; a kernel decision in plain words. */
function call(line: KernelRecordLine): string {
  return kernelDecision(line) ?? line.tool ?? "a call";
}

/** The form a call came from, by its tool: `send_invoice` is "the invoice form". */
function formOf(tool: string | undefined): string {
  const subject = tool?.split("_").slice(1).join(" ");
  return subject ? `the ${subject} form` : "a form";
}

/**
 * The sentence of an answer to a decision: yes or no to the call, whose call it was when the
 * record names the agent, and where: "You let Finance use send_email, from Telegram."
 */
function answer(line: KernelRecordLine, agents: AgentRow[]): string | undefined {
  // A decision the kernel asks is the person's own, whoever suggested it.
  const kernel = kernelDecision(line);
  if (kernel !== undefined) {
    switch (line.decision) {
      case "approved":
      case "edited":
        return `You said yes to ${kernel}${where(line)}.`;
      case "rejected":
        return `You said no to ${kernel}${where(line)}.`;
      default:
        return undefined;
    }
  }
  const agent = line.agent ? agentOf(line.agent, agents) : undefined;
  switch (line.decision) {
    case "approved":
    case "edited":
      return agent ? `You let ${agent} use ${call(line)}${where(line)}.` : `You said yes to ${call(line)}${where(line)}.`;
    case "rejected":
      return agent
        ? `You didn't let ${agent} use ${call(line)}${where(line)}.`
        : `You said no to ${call(line)}${where(line)}.`;
    default:
      return undefined;
  }
}

/**
 * A kernel record of an answer written before the answer is applied — a chat thread's
 * `decide`, a paired device's `approvals.decide` — which cannot say whether it decided.
 */
function answeredBefore(line: KernelRecordLine): boolean {
  return line.action.startsWith("kernel.") && (line.kernelAction === "decide" || line.kernelAction === "approvals.decide");
}

/** What the kernel's own record of a person's action did (`kernelAction`, spec 3.3 §10 K1–K5). */
function kernelSentence(line: KernelRecordLine, agents: AgentRow[]): string | undefined {
  switch (line.kernelAction) {
    case "approvals.decide":
    case "decide":
      return answer(line, agents);
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
      // An agent's screen action: its own record says the yes; a no changed nothing, and names
      // the item as the card did (an answer recorded before it did names none).
      if (line.tool === "verb_snooze" && line.outcome === "decided") {
        const agent = line.agent ? agentOf(line.agent, agents) : undefined;
        const what = line.title ?? "something in Coming up";
        return line.decision === "rejected"
          ? `${agent ?? "Your agent"} asked to snooze ${what}; you said no. Nothing changed.`
          : undefined;
      }
      return line.outcome === "decided" ? answer(line, agents) : undefined;
    case "surface.verb":
      return screenAction(line, agents);
    case "surface.stop_all":
      return `You stopped all agents${where(line)}.`;
    case "surface.resume_all":
      return `You resumed all agents${where(line)}.`;
    case "surface.set_mode":
      return "You changed how much your agents ask first.";
    // A form an agent drew, sent (spec 3.3 §7.4 rule 5): "You sent the invoice form Finance drew."
    case "surface.drawn.submitted":
      return `You sent ${formOf(line.tool)} ${(line.agent && agentOf(line.agent, agents)) ?? "an agent"} drew${where(line)}.`;
    // A rule refuses calls when its record says `denied`, and allows them otherwise.
    case "harness.rule.created":
      return line.decision === "denied"
        ? `You refused ${call(line)} without asking.`
        : `You allowed ${call(line)} without asking.`;
    case "harness.rule.revoked":
      return line.decision === "denied"
        ? `You stopped refusing ${call(line)} without asking.`
        : `You stopped allowing ${call(line)} without asking.`;
    case "harness.rule.extended":
      return line.decision === "denied"
        ? `You extended how long ${call(line)} is refused without asking.`
        : `You extended how long ${call(line)} is allowed without asking.`;
    default:
      break;
  }
  if (line.action.startsWith("kernel.")) return kernelSentence(line, agents);
  // A tool call: its action is the tool, and it names the agent that made it.
  if (line.tool !== undefined && line.action === line.tool && line.agent) {
    if (line.toolSource?.startsWith("verbs:")) return screenCall(line, agents);
    // The kernel's own decisions are not an agent's calls; their answers are on record.
    if (line.tool.startsWith("kernel:")) return undefined;
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

/**
 * The latest actions on record as sentences, newest first, at most `shown`, each answer once.
 * A kernel record of an answer written before it was applied is left out when the decision's
 * own answer that decided it is on record (`surface.decide`: a chat thread's tap writes both),
 * or when an earlier answer to the same decision is: that one decided, and this one changed
 * nothing.
 */
export function recentLines(
  lines: KernelRecordLine[],
  agents: AgentRow[],
  now: Date = new Date(),
  shown: number = RECORD_SHOWN,
): RecordLine[] {
  const decided = new Set(
    lines.filter((line) => line.action === "surface.decide" && line.outcome === "decided").flatMap((line) => line.target ?? []),
  );
  // Newest first, so the last pre-decision record of a decision seen is its earliest.
  const earliest = new Map<string, KernelRecordLine>();
  for (const line of lines) {
    if (answeredBefore(line) && line.target) earliest.set(line.target, line);
  }
  const out: RecordLine[] = [];
  for (const line of lines) {
    if (answeredBefore(line) && line.target && (decided.has(line.target) || earliest.get(line.target) !== line)) continue;
    const text = recordSentence(line, agents);
    if (!text) continue;
    const at = new Date(line.at);
    out.push({ seq: line.seq, at: line.at, atText: atText(at, now), text });
    if (out.length >= shown) break;
  }
  return out;
}

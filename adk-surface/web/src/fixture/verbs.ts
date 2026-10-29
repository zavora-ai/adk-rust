// Screen actions over the example (spec 3.3 §2 item 7) for `/ui/fixture.html?verbs`: News digest
// and Water plants in Coming up, Snooze on them, Finance's request to move News digest, and the
// record lines. `&verbs=cant` refuses the snooze, `&verbs=slow` keeps it on its way. Not part of
// the built page.
import type { AgendaItem, KernelProcess, KernelRecordLine } from "../kernel";
import type { DecisionCard, Origin } from "../generated/api";
import type { ComingUpItem, SnoozeChoice, VerbDone, VerbsResponse } from "../generated/verbs";
import { ApiError } from "../client";
import type { Seams } from "../seams";
import type { VerbsClient } from "../verbs/client";
import type { VoiceDevices } from "../voice/devices";
import { FixtureClient, agents, invoiceCard } from "./fixture";
import { FakeDevices } from "./voice";

/** Today at `hh:mm`, as an ISO time. */
function at(hh: number, mm = 0, dayOffset = 0): string {
  const date = new Date();
  date.setDate(date.getDate() + dayOffset);
  date.setHours(hh, mm, 0, 0);
  return date.toISOString();
}

function hhmm(iso: string): string {
  const date = new Date(iso);
  return `${String(date.getHours()).padStart(2, "0")}:${String(date.getMinutes()).padStart(2, "0")}`;
}

function weekdayTime(iso: string): string {
  return `${new Date(iso).toLocaleDateString("en-GB", { weekday: "short" })} ${hhmm(iso)}`;
}

/** The items Coming up holds, by id: title, agent and when each wakes. */
const ITEMS: Record<string, { title: string; target: string; at: string }> = {
  "agenda-7f3": { title: "News digest", target: "research", at: at(18) },
  "agenda-9c1": { title: "Water plants", target: "household", at: at(19) },
};

function choices(when: string): SnoozeChoice[] {
  const hour = new Date(new Date(when).getTime() + 3_600_000).toISOString();
  const day = new Date(new Date(when).getTime() + 24 * 3_600_000).toISOString();
  return [
    { to: hour, label: "1 hour later", atText: hhmm(hour), tomorrow: false },
    { to: day, label: "Tomorrow, same time", atText: weekdayTime(day), tomorrow: true },
  ];
}

/** The example's client with News digest and Water plants scheduled. */
export class VerbsHomeFixture extends FixtureClient {
  async processes(): Promise<KernelProcess[]> {
    // The example's own News digest routine gives way to the agenda item of that name.
    const titles = new Set(Object.values(ITEMS).map((item) => item.title));
    const routines = ((await super.processes()) ?? []).filter((process) => !titles.has(process.meta.labels?.title ?? ""));
    const scheduled: KernelProcess[] = Object.entries(ITEMS).map(([id, item]) => ({
      run: { runId: `p-${id}`, target: item.target, state: "waiting", input: { text: item.title } },
      meta: { pid: `p-${id}`, agendaItem: id, wake: { anyOf: [{ at: item.at }] }, labels: { title: item.title } },
    }));
    return [...routines, ...scheduled];
  }

  async agenda(): Promise<AgendaItem[]> {
    const open = (await super.agenda()) ?? [];
    const scheduled: AgendaItem[] = Object.entries(ITEMS).map(([id, item]) => ({
      id,
      title: item.title,
      origin: "user",
      state: { scheduled: { wake: { anyOf: [{ at: item.at }] } } },
      createdAt: at(9),
    }));
    return [...open, ...scheduled];
  }
}

/** The screen actions' routes over the example. */
export class VerbsFixture implements VerbsClient {
  constructor(private readonly how: string | null = null) {}

  async list(): Promise<VerbsResponse> {
    const comingUp: ComingUpItem[] = Object.entries(ITEMS).map(([id, item]) => ({
      id,
      title: item.title,
      area: item.target === "household" ? "home" : "work",
      at: item.at,
      atText: hhmm(item.at),
      choices: choices(item.at),
    }));
    return {
      verbs: [
        {
          name: "coming_up",
          label: "Coming up",
          description: "List what is coming up for the person.",
          objects: ["comingUp"],
          effect: "read",
          agentsAsk: true,
          onlyDelays: false,
          reversible: false,
        },
        {
          name: "snooze",
          label: "Snooze",
          description: "Snooze something in Coming up",
          objects: ["comingUp"],
          effect: "write_local",
          agentsAsk: true,
          onlyDelays: true,
          reversible: true,
        },
      ],
      comingUp,
    };
  }

  async snooze(item: ComingUpItem, choice: SnoozeChoice, origin: Origin): Promise<VerbDone> {
    void origin;
    if (this.how === "slow") await new Promise(() => undefined);
    if (this.how === "cant") throw new ApiError(409, "it has already started", "surface.verb.refused");
    return {
      verb: "snooze",
      target: `agenda:${item.id}`,
      title: item.title,
      text: `Moved ${item.title} to ${choice.atText}`,
      result: null,
    };
  }
}

/** Finance asks to move News digest (spec 3.3 §7.1): the card every surface draws. */
export function snoozeCard(): DecisionCard {
  const to = new Date(new Date(ITEMS["agenda-7f3"]!.at).getTime() + 3_600_000);
  const offset = -to.getTimezoneOffset();
  const sign = offset >= 0 ? "+" : "-";
  const zone = `${sign}${String(Math.floor(Math.abs(offset) / 60)).padStart(2, "0")}:${String(Math.abs(offset) % 60).padStart(2, "0")}`;
  const local = new Date(to.getTime() + offset * 60_000).toISOString().slice(0, 19) + zone;
  return {
    ...invoiceCard(),
    id: "apr_snooze",
    sessionId: "s-finance",
    agent: agents.finance,
    sinceText: "since 10:02",
    waitingSince: at(10, 2),
    heading: `Move News digest to ${hhmm(to.toISOString())}`,
    call: {
      tool: "verb_snooze",
      args: { item: "agenda-7f3", to: local },
      text: `verb_snooze(item: "agenda-7f3", to: "${local}")`,
      truncated: false,
    },
    timeline: [
      { text: "Finance read 1 web page (example.com)", current: false },
      { text: "Asked at 10:02", current: false },
      { text: "Waiting for you", current: true },
    ],
    allowed: {
      options: [
        { answer: "allowOnce", label: "Allow once", needsPresence: false, needsExpanded: false },
        { answer: "edit", label: "Edit", needsPresence: false, needsExpanded: false },
        { answer: "deny", label: "Deny", needsPresence: false, needsExpanded: false },
      ],
      openInHome: false,
    },
    risk: "low",
    riskText: "Low risk",
    reversibility: "canBeChangedBack",
    reversibilityText: "You can change it back",
    alsoShown: "Also on your phone",
    detail: { text: "Agents always ask before they change things on your screens." },
  };
}

/** The record lines of the concept, newest first, in place of the example's. */
function verbLines(): KernelRecordLine[] {
  const line = (seq: number, hh: number, mm: number, rest: Partial<KernelRecordLine>): KernelRecordLine => ({
    seq,
    at: at(hh, mm),
    action: "surface.verb",
    tool: "snooze",
    target: "News digest",
    outcome: "done",
    ...rest,
  });
  return [
    {
      seq: 905,
      at: at(10, 11),
      action: "surface.decide",
      tool: "verb_snooze",
      agent: "urn:adk:agent:home/finance",
      decision: "rejected",
      outcome: "decided",
      surface: "web",
      how: "pointer",
      target: "apr_905",
      title: "Water plants",
    },
    line(904, 10, 9, { agent: "urn:adk:agent:home/finance", decision: "approved", surface: "phone", how: "touch" }),
    line(903, 10, 7, { surface: "voice", how: "voice" }),
    line(902, 10, 6, { surface: "web", how: "pointer" }),
    line(901, 10, 5, { surface: "terminal", how: "keyboard" }),
  ];
}

/** `seams` with the record of the concept's screen actions. */
export function verbsSeams(seams: Seams): Seams {
  if (!seams.record) return seams;
  return { ...seams, record: { ...seams.record, lines: async () => verbLines() } };
}

/**
 * The voice view's server for a spoken snooze (spec 3.3 §5.6): `state` is `pending` (the
 * cancel window), `done`, `cancelled` or `ambiguous`.
 */
export function verbsVoiceDevices(state: string | null): VoiceDevices {
  const devices = new FakeDevices();
  const connect = devices.connect.bind(devices);
  const heading = `Move News digest to ${hhmm(new Date(new Date(ITEMS["agenda-7f3"]!.at).getTime() + 3_600_000).toISOString())}`;
  devices.connect = (url, events) => {
    const socket = connect(url, events);
    queueMicrotask(() => {
      devices.say({ type: "ready", sessionId: "voice-1", inputRate: 16_000, outputRate: 24_000 });
      if (state === "ambiguous") {
        devices.say({ type: "heard", text: "Snooze digest." });
        devices.say({
          type: "actionOutcome",
          done: false,
          text: "Two things match “digest” — say the full name or use the screen",
        });
        return;
      }
      devices.say({ type: "heard", text: "Snooze news digest." });
      devices.say({
        type: "pendingAction",
        actionId: "act-1",
        cancelUntil: new Date(Date.now() + 5_000).toISOString(),
        text: "Snoozing in 5 s",
        heading,
      });
      if (state === "done") {
        devices.say({
          type: "actionOutcome",
          actionId: "act-1",
          done: true,
          text: "You snoozed News digest by voice",
          heading: heading.replace("Move", "Moved"),
        });
      } else if (state === "cancelled") {
        devices.say({ type: "actionOutcome", actionId: "act-1", done: false, text: "Cancelled · nothing changed", heading });
      }
    });
    return socket;
  };
  return devices;
}

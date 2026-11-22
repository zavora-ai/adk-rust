import { visible, visibleJson } from "../safe";
import type { RuntimeEvent, TopologyMember, TraceSpan } from "./types";

/**
 * One run of a session as the developer view draws it (spec 3.3 §12 developer track): a tree of
 * steps — agents, the agents they hand work to, model calls and tool calls — on one clock, so the
 * graph, the tree and the trace read the same run. It comes from the session's trace spans where
 * telemetry collects them, else from the session's events, which every server keeps.
 */
export type StepKind = "agent" | "handoff" | "model" | "tool" | "held" | "flow" | "person" | "step";

export type Step = {
  id: string;
  parent?: string;
  depth: number;
  kind: StepKind;
  label: string;
  /** The agent the step ran under. */
  agent?: string;
  /** Milliseconds from the run's start. */
  start: number;
  /** Milliseconds from the run's start; none while the step still runs. */
  end?: number;
  tokens?: { input: number; output: number };
  /** What went in or came out, already made inert. */
  detail?: { label: string; text: string };
  attrs: Array<[string, string]>;
};

export type Run = {
  source: "trace" | "events";
  steps: Step[];
  /** The run's length in milliseconds, up to now for a live run. */
  total: number;
  agents: string[];
  tokens: { input: number; output: number };
  live: boolean;
};

export type MemberState = "idle" | "active" | "held" | "done";

const DETAIL_LIMIT = 4000;

/** `text` cut to the detail limit, inert. */
function clip(text: string): string {
  const shown = text.length > DETAIL_LIMIT ? `${text.slice(0, DETAIL_LIMIT)}…` : text;
  return visible(shown);
}

function pretty(value: unknown): string {
  if (typeof value === "string") {
    try {
      return visibleJson(JSON.stringify(JSON.parse(value), null, 2));
    } catch {
      return clip(value);
    }
  }
  const json = JSON.stringify(value, null, 2) ?? "";
  return visibleJson(json.length > DETAIL_LIMIT ? `${json.slice(0, DETAIL_LIMIT)}…` : json);
}

function number(text: string | undefined): number | undefined {
  if (text === undefined) return undefined;
  const value = Number(text);
  return Number.isFinite(value) ? value : undefined;
}

/** Attributes for the inspector: the semantic ones first, the bookkeeping last. */
function shownAttributes(attributes: Record<string, string>): Array<[string, string]> {
  const hidden = new Set(["start_time", "end_time", "span_name", "gcp.vertex.agent.llm_request", "gcp.vertex.agent.llm_response"]);
  const rank = (key: string) => (key.startsWith("gen_ai.") ? 0 : key.startsWith("adk.") || key.includes(".") ? 1 : 2);
  return Object.entries(attributes)
    .filter(([key]) => !hidden.has(key))
    .sort(([a], [b]) => rank(a) - rank(b) || a.localeCompare(b))
    .map(([key, value]) => [visible(key), clip(value)]);
}

type Shaped = { kind: StepKind; label: string; agent?: string };

/** Spans of one model call: the agent's `call_llm`, the model's `model.call` and its generation. */
const MODEL_SPANS = new Set(["call_llm", "model.call", "gen_ai.generate"]);

/** A tool call still waiting for its reply, from the session's events. */
export type OpenCall = { id: string; name: string; agent?: string; at: number; held: boolean; args: unknown };

/**
 * What is still running, from the session's events: the tool calls with no reply (one asking
 * for confirmation is held for a person), and the agents whose spans have not ended because of
 * them — the invocation's first agent from the person's message, and each agent with an open call
 * from when work was handed to it. Times are epoch milliseconds.
 */
export type OpenWork = { calls: OpenCall[]; agents: Array<{ agent: string; at: number }> };

export function openWork(events: RuntimeEvent[]): OpenWork {
  const open = new Map<string, OpenCall>();
  for (const event of events) {
    const at = event.timestamp ? Date.parse(event.timestamp) : NaN;
    const parts = (event.content?.parts ?? []) as Part[];
    for (const call of calls(parts)) {
      if (!Number.isFinite(at)) continue;
      const held = Boolean(event.actions?.tool_confirmation) || event.event_metadata?.["adk.long_running"] === "true";
      open.set(call.id ?? call.name, { id: `${event.id}-${call.id ?? call.name}`, name: call.name, agent: event.author, at, held, args: call.args });
    }
    for (const reply of responses(parts)) open.delete(reply.id ?? reply.name);
  }
  const pending = [...open.values()];
  if (pending.length === 0) return { calls: [], agents: [] };
  const latest = [...events].reverse().find((event) => event.invocation_id)?.invocation_id;
  const turn = events.filter((event) => event.invocation_id === latest && event.timestamp);
  const time = (event?: RuntimeEvent) => (event?.timestamp ? Date.parse(event.timestamp) : NaN);
  const agents: Array<{ agent: string; at: number }> = [];
  const first = turn.find((event) => event.author && event.author !== "user");
  const started = time(turn[0]);
  if (first?.author && Number.isFinite(started)) agents.push({ agent: first.author, at: started });
  for (const call of pending) {
    if (!call.agent || agents.some((entry) => entry.agent === call.agent)) continue;
    const handed = turn.find((event) => event.actions?.transfer_to_agent === call.agent);
    const own = turn.find((event) => event.author === call.agent);
    const at = Number.isFinite(time(handed)) ? time(handed) : time(own);
    if (Number.isFinite(at)) agents.push({ agent: call.agent, at });
  }
  return { calls: pending, agents };
}

function shape(span: TraceSpan): Shaped {
  const a = span.attributes;
  const operation = a["gen_ai.operation.name"];
  const agent = a["gen_ai.agent.name"] || a["agent.name"] || undefined;
  if (span.name === "agent.run" || operation === "invoke_agent") return { kind: "agent", label: agent ?? span.name, agent };
  if (span.name === "team.run") return { kind: "agent", label: a["team.name"] || "team", agent: a["team.coordinator"] || a["team.name"] };
  if (span.name === "team.member.run") return { kind: "handoff", label: a["team.member"] || "member", agent: a["team.member"] };
  if (span.name === "tool.execute" || operation === "execute_tool") {
    return { kind: "tool", label: a["gen_ai.tool.name"] || a["tool.name"] || span.name, agent };
  }
  if (MODEL_SPANS.has(span.name) || a["gen_ai.request.model"]) {
    return { kind: "model", label: a["gen_ai.request.model"] || a["model.name"] || "model call", agent };
  }
  if (a["gcp.vertex.agent.event_id"]?.includes("->")) return { kind: "flow", label: a["gcp.vertex.agent.event_id"], agent };
  return { kind: "step", label: span.name, agent };
}

/**
 * The run in a session's trace spans. The server sends them flat, with no parents, so a span's
 * parent is the shortest span that holds it in time; a model call wrapped around one generation
 * folds into it, and an agent that runs inside another agent is a handoff.
 */
export function runFromSpans(spans: TraceSpan[], now = Date.now(), work: OpenWork = { calls: [], agents: [] }): Run {
  const pending = work.calls;
  // An agent still running has no span yet; it stands in, open, from when it started.
  const standIns: TraceSpan[] = work.agents.map(({ agent, at }) => ({
    name: "agent.execute",
    span_id: `open-${agent}`,
    trace_id: "",
    start_time: at * 1_000_000,
    end_time: 0,
    attributes: { "gen_ai.operation.name": "invoke_agent", "gen_ai.agent.name": agent, "adk.source": "session events (still running)" },
  }));
  const valid = [...spans.filter((span) => span.start_time > 0), ...standIns];
  if (valid.length === 0) return emptyRun("trace");
  const origin = Math.min(...valid.map((span) => span.start_time), ...pending.map((call) => call.at * 1_000_000));
  const nowNs = now * 1_000_000;
  const timed = valid
    .map((span) => {
      const running = !(span.end_time > span.start_time);
      return { span, shaped: shape(span), start: span.start_time, end: running ? undefined : span.end_time };
    })
    .sort((a, b) => a.start - b.start || (b.end ?? Infinity) - (a.end ?? Infinity));

  const holds = (outer: (typeof timed)[number], inner: (typeof timed)[number]) =>
    outer.start <= inner.start && (outer.end === undefined || (inner.end !== undefined && inner.end <= outer.end));

  // An agent's call_llm or a model.call that holds a generation is the same call: the generation
  // stays, with the wrapper's attributes (its response) under its own.
  const folded = timed.filter((item) => {
    if (item.span.name !== "call_llm" && item.span.name !== "model.call") return true;
    const inner = timed.find((other) => other !== item && other.span.name === "gen_ai.generate" && holds(item, other));
    if (!inner) return true;
    inner.span = { ...inner.span, attributes: { ...item.span.attributes, ...inner.span.attributes } };
    return false;
  });

  const steps: Step[] = [];
  const stack: Array<{ item: (typeof folded)[number]; step: Step }> = [];
  for (const item of folded) {
    while (stack.length > 0 && !holds(stack[stack.length - 1].item, item)) stack.pop();
    const parent = stack[stack.length - 1]?.step;
    const a = item.span.attributes;
    let { kind, label } = item.shaped;
    const inherited = item.shaped.agent ?? parent?.agent;
    if (kind === "agent" && parent && parent.agent && parent.agent !== item.shaped.agent) kind = "handoff";
    if (kind === "handoff") label = `→ ${label}`;
    const input = number(a["gen_ai.usage.input_tokens"]);
    const output = number(a["gen_ai.usage.output_tokens"]);
    const args = a["gen_ai.tool.call.arguments"];
    const result = a["gen_ai.tool.call.result"];
    const response = a["gcp.vertex.agent.llm_response"];
    const step: Step = {
      id: item.span.span_id || `${item.span.name}-${item.start}`,
      parent: parent?.id,
      depth: parent ? parent.depth + 1 : 0,
      kind,
      label: visible(label),
      agent: inherited ? visible(inherited) : undefined,
      start: (item.start - origin) / 1_000_000,
      end: item.end === undefined ? undefined : (item.end - origin) / 1_000_000,
      tokens: input !== undefined || output !== undefined ? { input: input ?? 0, output: output ?? 0 } : undefined,
      detail:
        result !== undefined
          ? { label: "Result", text: pretty(result) }
          : args !== undefined
            ? { label: "Arguments", text: pretty(args) }
            : response !== undefined
              ? { label: "Response", text: pretty(response) }
              : undefined,
      attrs: shownAttributes(a),
    };
    steps.push(step);
    stack.push({ item, step });
  }
  // Spans arrive when they end, so a call still waiting comes from the session's events.
  for (const call of pending) {
    const under = [...steps].reverse().find((step) => step.agent === call.agent && (step.kind === "agent" || step.kind === "handoff"));
    steps.push({
      id: call.id,
      parent: under?.id,
      depth: under ? under.depth + 1 : 0,
      kind: call.held ? "held" : "tool",
      label: visible(call.name),
      agent: call.agent ? visible(call.agent) : undefined,
      start: (call.at * 1_000_000 - origin) / 1_000_000,
      end: undefined,
      detail: { label: "Arguments", text: pretty(call.args) },
      attrs: [["gen_ai.tool.name", visible(call.name)]],
    });
  }
  return finish("trace", steps, (nowNs - origin) / 1_000_000);
}

type Part = Record<string, unknown>;

function text(parts: Part[]): string {
  return parts
    .map((part) => (typeof part.text === "string" ? part.text : ""))
    .filter(Boolean)
    .join("\n")
    .trim();
}

function calls(parts: Part[]): Array<{ id?: string; name: string; args: unknown }> {
  return parts.flatMap((part) => {
    const call = (part.functionCall ?? part.function_call) as { id?: string; name?: string; args?: unknown } | undefined;
    return call?.name ? [{ id: call.id, name: call.name, args: call.args ?? {} }] : [];
  });
}

function responses(parts: Part[]): Array<{ id?: string; name: string; response: unknown }> {
  return parts.flatMap((part) => {
    const reply = (part.functionResponse ?? part.function_response) as { id?: string; name?: string; response?: unknown } | undefined;
    return reply?.name ? [{ id: reply.id, name: reply.name, response: reply.response ?? {} }] : [];
  });
}

function short(line: string, length = 64): string {
  const single = line.replace(/\s+/g, " ").trim();
  return single.length > length ? `${single.slice(0, length - 1)}…` : single;
}

/**
 * The run in a session's events, for a server without telemetry: each invocation, the agents
 * that spoke in it in turn (an agent after the first is a handoff), and under each its tool calls
 * and replies. A model reply takes the time since the step before it; a tool call lasts until its
 * reply, and one held for a person's yes has no end yet.
 */
export function runFromEvents(events: RuntimeEvent[], now = Date.now()): Run {
  const timed = events
    .filter((event) => event.timestamp && !event.partial)
    .map((event) => ({ event, at: Date.parse(event.timestamp as string) }))
    .filter((item) => Number.isFinite(item.at))
    .sort((a, b) => a.at - b.at);
  if (timed.length === 0) return emptyRun("events");
  const origin = timed[0].at;
  const steps: Step[] = [];
  const open = new Map<string, Step>();
  let root: Step | undefined;
  let invocation: string | undefined;
  let speaker: Step | undefined;
  let previous = 0;
  for (const { event, at } of timed) {
    const t = at - origin;
    const author = event.author ?? "agent";
    const parts = (event.content?.parts ?? []) as Part[];
    if (event.invocation_id !== invocation || !root) {
      invocation = event.invocation_id;
      root = undefined;
      speaker = undefined;
    }
    if (author === "user") {
      const said = text(parts);
      steps.push({
        id: event.id,
        depth: 0,
        kind: "person",
        label: visible(short(said ? `You: ${said}` : "You")),
        start: t,
        end: t,
        detail: said ? { label: "Message", text: clip(said) } : undefined,
        attrs: [["event.id", visible(event.id)], ...(event.invocation_id ? [["invocation.id", visible(event.invocation_id)] as [string, string]] : [])],
      });
      previous = t;
      continue;
    }
    if (!root) {
      root = { id: `run-${event.invocation_id ?? event.id}`, depth: 0, kind: "agent", label: visible(author), agent: visible(author), start: previous, end: t, attrs: event.invocation_id ? [["invocation.id", visible(event.invocation_id)]] : [] };
      steps.push(root);
      speaker = root;
    }
    if (visible(author) === root.agent) {
      speaker = root;
    } else if (speaker?.agent !== visible(author)) {
      speaker = { id: `handoff-${event.id}`, parent: root.id, depth: 1, kind: "handoff", label: visible(`→ ${author}`), agent: visible(author), start: previous, end: t, attrs: event.branch ? [["branch", visible(event.branch)]] : [] };
      steps.push(speaker);
    }
    const under = speaker ?? root;
    const delta = event.actions?.state_delta;
    const facts: Array<[string, string]> = [
      ["event.id", visible(event.id)],
      ...(event.branch ? [["branch", visible(event.branch)] as [string, string]] : []),
      ...(delta && Object.keys(delta).length > 0 ? [["state_delta", pretty(delta)] as [string, string]] : []),
    ];
    const said = text(parts);
    if (said) {
      steps.push({ id: `${event.id}-text`, parent: under.id, depth: under.depth + 1, kind: "model", label: visible(short(said)), agent: under.agent, start: previous, end: t, detail: { label: "Reply", text: clip(said) }, attrs: facts });
    }
    for (const call of calls(parts)) {
      const held = Boolean(event.actions?.tool_confirmation) || event.event_metadata?.["adk.long_running"] === "true";
      const step: Step = { id: `${event.id}-${call.id ?? call.name}`, parent: under.id, depth: under.depth + 1, kind: held ? "held" : "tool", label: visible(call.name), agent: under.agent, start: t, end: undefined, detail: { label: "Arguments", text: pretty(call.args) }, attrs: facts };
      steps.push(step);
      open.set(call.id ?? call.name, step);
    }
    for (const reply of responses(parts)) {
      const step = open.get(reply.id ?? reply.name);
      if (step) {
        step.end = t;
        step.kind = "tool";
        step.detail = { label: "Result", text: pretty(reply.response) };
        open.delete(reply.id ?? reply.name);
      }
    }
    const transfer = event.actions?.transfer_to_agent;
    if (transfer) {
      steps.push({ id: `${event.id}-transfer`, parent: under.id, depth: under.depth + 1, kind: "handoff", label: visible(`hands off to ${transfer}`), agent: under.agent, start: t, end: t, attrs: facts });
    }
    if (!said && calls(parts).length === 0 && responses(parts).length === 0 && !transfer && event.error_message) {
      steps.push({ id: `${event.id}-error`, parent: under.id, depth: under.depth + 1, kind: "step", label: visible(short(event.error_message)), agent: under.agent, start: t, end: t, attrs: facts });
    }
    under.end = Math.max(under.end ?? t, t);
    root.end = Math.max(root.end ?? t, t);
    previous = t;
  }
  // A step still open keeps its agent, and the run, open too.
  for (const step of open.values()) {
    for (let parent = steps.find((s) => s.id === step.parent); parent; parent = steps.find((s) => s.id === parent?.parent)) parent.end = undefined;
  }
  return finish("events", steps, now - origin);
}

function emptyRun(source: Run["source"]): Run {
  return { source, steps: [], total: 0, agents: [], tokens: { input: 0, output: 0 }, live: false };
}

function finish(source: Run["source"], steps: Step[], elapsed: number): Run {
  const live = steps.some((step) => step.end === undefined);
  const last = Math.max(0, ...steps.map((step) => step.end ?? step.start));
  const total = Math.max(1, live ? Math.max(last, elapsed) : last);
  const agents = [...new Set(steps.map((step) => step.agent).filter((agent): agent is string => Boolean(agent)))];
  const tokens = steps.reduce(
    (sum, step) => ({ input: sum.input + (step.tokens?.input ?? 0), output: sum.output + (step.tokens?.output ?? 0) }),
    { input: 0, output: 0 },
  );
  return { source, steps, total, agents, tokens, live };
}

/** How each member of the topology stands in the run: held for a person, still working, done or idle. */
export function memberStates(run: Run, members: TopologyMember[]): Map<string, { state: MemberState; time?: number }> {
  const states = new Map<string, { state: MemberState; time?: number }>();
  for (const member of members) {
    const own = run.steps.filter((step) => step.agent === member.name);
    if (own.length === 0) {
      states.set(member.name, { state: "idle" });
      continue;
    }
    const start = Math.min(...own.map((step) => step.start));
    const end = Math.max(...own.map((step) => step.end ?? run.total));
    const state: MemberState = own.some((step) => step.kind === "held" && step.end === undefined)
      ? "held"
      : own.some((step) => step.end === undefined)
        ? "active"
        : "done";
    states.set(member.name, { state, time: end - start });
  }
  return states;
}

/** Steps a person can see: none under a folded step. */
export function shownSteps(steps: Step[], folded: ReadonlySet<string>): Step[] {
  const hidden = new Set<string>();
  return steps.filter((step) => {
    if (step.parent && (hidden.has(step.parent) || folded.has(step.parent))) {
      hidden.add(step.id);
      return false;
    }
    return true;
  });
}

/** A duration for people: milliseconds under a second, then seconds, then minutes. */
export function duration(ms: number): string {
  if (ms < 1) return "<1 ms";
  if (ms < 1000) return `${Math.round(ms)} ms`;
  if (ms < 60_000) return `${(ms / 1000).toFixed(ms < 10_000 ? 2 : 1)} s`;
  const minutes = Math.floor(ms / 60_000);
  return `${minutes} min ${Math.round((ms % 60_000) / 1000)} s`;
}

/** A tick on the trace's clock: 0, 200 ms, 10 s, 2.5 s. */
export function tickText(ms: number): string {
  if (ms === 0) return "0";
  if (ms < 1000) return `${Math.round(ms)} ms`;
  if (ms < 60_000) return `${Number((ms / 1000).toFixed(2))} s`;
  return duration(ms);
}

/** Four or five round ticks across `total` milliseconds, for the trace's clock. */
export function ticks(total: number): number[] {
  const raw = total / 4;
  const power = 10 ** Math.floor(Math.log10(Math.max(raw, 1e-3)));
  const step = [1, 2, 2.5, 5, 10].map((m) => m * power).find((candidate) => candidate >= raw) ?? raw;
  const out: number[] = [];
  for (let t = 0; t <= total + 1e-9; t += step) out.push(t);
  return out;
}

/** A token count for people: 6,120 reads as 6.1k. */
export function tokensText(count: number): string {
  return count >= 1000 ? `${(count / 1000).toFixed(count >= 10_000 ? 0 : 1)}k` : String(count);
}

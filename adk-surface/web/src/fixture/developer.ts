// The developer view's example (spec 3.3 §12 developer track): Operations hands the measuring
// to Metrics, then the rollback to Deployer, whose restart_service waits for the person. The
// trace holds only spans that ended, as the server's exporter sends them; the open call comes
// from the session's events. Not part of the built page.
import { ApiError } from "../client";
import type { AgentDetails, RuntimeEvent, Session, TraceSpan } from "../developer/types";
import { FixtureClient } from "./fixture";

const CAPS = { runtimeTools: true, handoff: true, relationshipConfirmation: false, checkpointResume: false, sharedState: true, invocationMetadata: true };

export function operationsAgent(): AgentDetails {
  return {
    name: "operations",
    description: "Keeps checkout healthy: measures, then fixes with your yes.",
    kind: "team",
    interactionMode: "requestResponse",
    capabilities: CAPS,
    services: { telemetry: true, telemetryStatus: "collecting", artifacts: false, memory: true },
    children: [],
    topology: {
      root: "operations",
      coordinator: "operations",
      members: [
        { name: "operations", description: "Coordinates the investigation and the fix.", coordinator: true, capabilities: CAPS },
        { name: "metrics", description: "Reads latency and deploys; never changes anything.", coordinator: false, capabilities: { ...CAPS, handoff: false } },
        { name: "deployer", description: "Rolls services back and forward, with your yes.", coordinator: false, capabilities: CAPS },
      ],
      relationships: [
        { from: "operations", to: "metrics", kind: "delegate" },
        { from: "operations", to: "deployer", kind: "handoff" },
      ],
    },
  };
}

/** The run started `ago` seconds before `now`. */
function clock(now: number, ago: number) {
  const origin = now - ago * 1000;
  return {
    ns: (s: number) => Math.round((origin + s * 1000) * 1_000_000),
    iso: (s: number) => new Date(origin + s * 1000).toISOString(),
  };
}

function span(name: string, id: string, start: number, end: number, attributes: Record<string, string>, ns: (s: number) => number): TraceSpan {
  return {
    name,
    span_id: id,
    trace_id: "inv-7f3a",
    start_time: ns(start),
    end_time: ns(end),
    invoc_id: "inv-7f3a",
    attributes: { "gcp.vertex.agent.invocation_id": "inv-7f3a", "gen_ai.conversation.id": "s-ops", span_name: name, span_id: id, trace_id: "inv-7f3a", ...attributes },
  };
}

function model(id: string, start: number, end: number, name: string, input: number, output: number, response: string, ns: (s: number) => number): TraceSpan[] {
  return [
    span("call_llm", `${id}-call`, start, end, { "gcp.vertex.agent.llm_response": response }, ns),
    span("gen_ai.generate", id, start + 0.02, end - 0.02, {
      "gen_ai.operation.name": "chat",
      "gen_ai.provider.name": "anthropic",
      "gen_ai.request.model": name,
      "gen_ai.usage.input_tokens": String(input),
      "gen_ai.usage.output_tokens": String(output),
      "gen_ai.response.finish_reasons": '["tool_use"]',
    }, ns),
  ];
}

function tool(id: string, start: number, end: number, name: string, agent: string, args: string, result: string, ns: (s: number) => number): TraceSpan {
  return span("", id, start, end, {
    "gen_ai.operation.name": "execute_tool",
    "gen_ai.tool.name": name,
    "gen_ai.tool.type": "function",
    "gen_ai.agent.name": agent,
    "gen_ai.tool.call.arguments": args,
    "gen_ai.tool.call.result": result,
  }, ns);
}

/** The ended spans of the run, 38 seconds in. */
export function operationsTrace(now = Date.now()): TraceSpan[] {
  const { ns } = clock(now, 38.4);
  return [
    ...model("g1", 0.1, 2.6, "claude-opus-5-5", 1240, 186, "Delegate the measurement to Metrics, then decide on a rollback.", ns),
    span("agent.execute", "metrics-run", 2.7, 12.5, { "gen_ai.operation.name": "invoke_agent", "gen_ai.agent.name": "metrics", "agent.name": "metrics" }, ns),
    ...model("g2", 2.8, 4.7, "claude-sonnet-5-5", 820, 94, 'query_p95(window: "3h"), read_deploys(service: "checkout-api")', ns),
    tool("t1", 4.8, 6.9, "query_p95", "metrics", '{"window":"3h","services":["checkout","search"]}', '{"checkout":{"before":410,"after":840},"search":{"before":402,"after":398}}', ns),
    tool("t2", 7.0, 7.8, "read_deploys", "metrics", '{"service":"checkout-api"}', '[{"at":"09:12","version":"2026.11.16-2"}]', ns),
    ...model("g3", 7.9, 12.3, "claude-sonnet-5-5", 1610, 212, "checkout p95 410 → 840 ms after the 09:12 deploy; search flat at ~400 ms.", ns),
    ...model("g4", 12.6, 15.6, "claude-opus-5-5", 1980, 240, "Draw the latency, then hand the rollback to Deployer.", ns),
    tool("t3", 15.7, 15.9, "render_screen", "operations", '{"surfaceId":"latency","components":6}', '{"version":1,"warnings":0}', ns),
    ...model("g5", 16.1, 17.7, "claude-sonnet-5-5", 640, 88, 'restart_service(service: "checkout-api", to: "2026.11.16-1")', ns),
  ];
}

/** The session's events: the person's message, the handoffs and the call that waits. */
export function operationsEvents(now = Date.now()): RuntimeEvent[] {
  const { iso } = clock(now, 38.4);
  const event = (id: string, s: number, author: string, parts: Array<Record<string, unknown>>, actions?: RuntimeEvent["actions"]): RuntimeEvent => ({
    id,
    timestamp: iso(s),
    invocation_id: "inv-7f3a",
    author,
    content: { role: author === "user" ? "user" : "model", parts },
    actions,
  });
  return [
    event("e1", 0, "user", [{ text: "Checkout feels slow since this morning. Find out why, and roll it back if that's the fix." }]),
    event("e2", 2.6, "operations", [{ text: "Asking Metrics to measure first." }], { transfer_to_agent: "metrics" }),
    event("e3", 12.4, "metrics", [{ text: "checkout p95 went from 410 to 840 ms after the 09:12 deploy." }]),
    event("e4", 15.9, "operations", [{ text: "Handing the rollback to Deployer." }], { transfer_to_agent: "deployer", state_delta: { suspect: "checkout-api 2026.11.16-2" } }),
    event("e5", 17.8, "deployer", [{ functionCall: { id: "call-restart", name: "restart_service", args: { service: "checkout-api", to: "2026.11.16-1" } } }], { tool_confirmation: { hint: "restart 6 pods" } }),
  ];
}

/** The home's example with the developer routes of a server that collects telemetry. */
export class DeveloperFixture extends FixtureClient {
  override async get<T>(path?: string): Promise<T> {
    if (path === "/apps") return ["operations"] as T;
    if (path?.startsWith("/ui/agents/")) return operationsAgent() as T;
    if (path?.startsWith("/debug/trace/session/")) return operationsTrace() as T;
    if (path?.startsWith("/apps/operations/users/")) {
      const session: Session = { id: "s-ops", appName: "operations", userId: "you", lastUpdateTime: Date.now() / 1000, events: operationsEvents(), state: { suspect: "checkout-api 2026.11.16-2" } };
      return session as T;
    }
    if (path?.startsWith("/ui/capabilities")) return { protocols: [] } as T;
    throw new ApiError(404, "not in the fixture");
  }
}

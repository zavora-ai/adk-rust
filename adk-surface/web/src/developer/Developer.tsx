import { useEffect, useState } from "react";
import type { HomeClient } from "../client";
import type { GraphView } from "../generated/workflow";
import type { WorkflowClient } from "../workflow/client";
import { workflowLabels } from "../workflow/labels";
import { runsOf } from "../workflow/route";
import { Runs } from "../workflow/Runs";
import { topologyActivity } from "./topologyActivity";
import type { A2aCard, AgentDetails, RuntimeEvent, Session, TopologyMember, TopologyRelationship, TraceSpan, UiCapabilities } from "./types";

type Tab = "topology" | "protocols" | "telemetry" | "state" | "runs";

/** The tabs' labels, in sentence case (spec 3.3 §8). */
const TAB_LABELS: Record<Tab, string> = {
  topology: "Topology",
  protocols: "Protocols",
  telemetry: "Telemetry",
  state: "State",
  runs: workflowLabels.tab,
};

const USER_KEY = "adk.home.developer.user";

function storedUser(): string {
  try {
    return window.localStorage.getItem(USER_KEY) ?? "";
  } catch {
    return "";
  }
}

/**
 * The developer panel (spec 3.3 §4, §12 developer track): what the runtime UI that `adk-server`
 * bundled showed and is worth keeping — the agent's topology, the runtime services and UI
 * protocols its server advertises, a session's telemetry and state — on the server's own
 * APIs. Developer panels may use technical words (§8).
 */
export function Developer({
  client,
  sessionId,
  workflow,
}: {
  client: HomeClient;
  sessionId?: string;
  /** The workflow observer's routes and graphs, where the server serves them (spec 3.3 §12). */
  workflow?: { client: WorkflowClient; graphs: GraphView[] };
}) {
  // `/ui/developer/runs/<session>` opens the Runs tab on that session ("Open in Runs").
  const [runsPlace] = useState(() => runsOf(window.location));
  const [waiting, setWaiting] = useState(0);
  const [apps, setApps] = useState<string[]>([]);
  const [app, setApp] = useState("");
  const [agent, setAgent] = useState<AgentDetails>();
  const [protocols, setProtocols] = useState<UiCapabilities>();
  const [a2a, setA2a] = useState<{ card: A2aCard; path: string }>();
  const [traces, setTraces] = useState<TraceSpan[]>([]);
  const [user, setUser] = useState(storedUser);
  const [session, setSession] = useState<Session>();
  const [tab, setTab] = useState<Tab>(runsPlace ? "runs" : "topology");
  const [problem, setProblem] = useState("");

  useEffect(() => {
    void Promise.all([
      client.get<string[]>("/apps").catch(() => [] as string[]),
      client.get<UiCapabilities>("/ui/capabilities").catch(() => undefined),
      a2aCard().catch(() => undefined),
    ]).then(([names, capabilities, card]) => {
      const sorted = [...names].sort();
      setApps(sorted);
      setApp((current) => current || sorted[0] || "");
      setProtocols(capabilities);
      setA2a(card);
    });
  }, [client]);

  useEffect(() => {
    if (!app) return;
    client
      .get<AgentDetails>(`/ui/agents/${encodeURIComponent(app)}`)
      .then(setAgent)
      .catch((error: unknown) => setProblem(error instanceof Error ? error.message : String(error)));
  }, [client, app]);

  useEffect(() => {
    if (!sessionId) return;
    client.get<TraceSpan[]>(`/debug/trace/session/${encodeURIComponent(sessionId)}`).then(setTraces).catch(() => setTraces([]));
    if (app && user) {
      client
        .get<Session>(`/apps/${encodeURIComponent(app)}/users/${encodeURIComponent(user)}/sessions/${encodeURIComponent(sessionId)}`)
        .then(setSession)
        .catch(() => setSession(undefined));
    }
  }, [client, sessionId, app, user]);

  const tabs: Tab[] = ["topology", "protocols", "telemetry", "state", ...(workflow ? (["runs"] as const) : [])];
  const runsSession = runsPlace?.session ?? sessionId;
  return (
    <main className="developer">
      <header className="developer-head">
        <h1 className="developer-title">Developer panel</h1>
        <label>
          Agent
          <select value={app} onChange={(event) => setApp(event.target.value)}>
            {apps.map((name) => (
              <option key={name}>{name}</option>
            ))}
          </select>
        </label>
        <label>
          User ID
          <input
            value={user}
            placeholder="for session state"
            onChange={(event) => {
              setUser(event.target.value);
              try {
                window.localStorage.setItem(USER_KEY, event.target.value);
              } catch {
                // Storage can be off; the value lasts for this page only.
              }
            }}
          />
        </label>
        {(tab === "runs" ? runsSession : sessionId) && (
          <span className="mono muted">session {tab === "runs" ? runsSession : sessionId}</span>
        )}
      </header>
      {problem && (
        <p className="decision-problem" role="alert">
          {problem}
        </p>
      )}
      <nav className="dev-tabs" aria-label="Developer panel">
        {tabs.map((name) => (
          <button key={name} type="button" aria-pressed={tab === name} onClick={() => setTab(name)}>
            {TAB_LABELS[name]}
            {name === "runs" && waiting > 0 && <span className="tab-badge">{waiting}</span>}
          </button>
        ))}
      </nav>
      <section className="dev-content">
        {tab === "topology" && <Topology agent={agent} events={session?.events ?? []} />}
        {tab === "runs" && workflow && (
          <Runs
            client={workflow.client}
            home={client}
            graphs={workflow.graphs}
            agent={app}
            session={runsSession}
            onWaiting={setWaiting}
          />
        )}
        {tab === "protocols" && <Protocols agent={agent} protocols={protocols} a2a={a2a} traceCount={traces.length} />}
        {tab === "telemetry" &&
          (traces.length === 0 ? (
            <p className="muted">No spans for this session.</p>
          ) : (
            <div className="trace-list">
              {traces.map((trace) => (
                <details key={trace.span_id}>
                  <summary>
                    <strong>{trace.name}</strong>
                    <span className="mono">
                      {trace.end_time > trace.start_time ? `${((trace.end_time - trace.start_time) / 1_000_000).toFixed(1)} ms` : "active"}
                    </span>
                  </summary>
                  <pre>{JSON.stringify(trace.attributes, null, 2)}</pre>
                </details>
              ))}
            </div>
          ))}
        {tab === "state" &&
          (!session ? (
            <p className="muted">Enter the user ID to read this session's state.</p>
          ) : (
            <div className="data-list">
              {Object.entries(session.state).map(([key, value]) => (
                <details key={key}>
                  <summary className="mono">{key}</summary>
                  <pre>{JSON.stringify(value, null, 2)}</pre>
                </details>
              ))}
            </div>
          ))}
      </section>
    </main>
  );
}

async function a2aCard(): Promise<{ card: A2aCard; path: string } | undefined> {
  for (const path of ["/.well-known/agent.json", "/.well-known/agent-card.json"]) {
    const response = await fetch(path);
    if (response.ok) return { card: (await response.json()) as A2aCard, path };
  }
  return undefined;
}

function Topology({ agent, events }: { agent?: AgentDetails; events: RuntimeEvent[] }) {
  if (!agent) return <p className="muted">Select an agent to see its topology.</p>;
  const members: TopologyMember[] = agent.topology?.members ?? [
    { name: agent.name, description: agent.description, coordinator: true, capabilities: agent.capabilities },
    ...agent.children.map((child) => ({ ...child, coordinator: false })),
  ];
  const relationships: Array<TopologyRelationship | { from: string; to: string; kind: "contains" }> =
    agent.topology?.relationships ?? agent.children.map((child) => ({ from: agent.name, to: child.name, kind: "contains" as const }));
  const width = 560;
  const coordinator = members.find((member) => member.coordinator) ?? members[0];
  if (!coordinator) return null;
  const { activeMember, activeEdge } = topologyActivity(members, events, false);
  const others = members.filter((member) => member.name !== coordinator.name);
  const positions = new Map<string, { x: number; y: number }>();
  const hasFlow = relationships.some((edge) => edge.kind === "flow");
  let height: number;
  if (hasFlow) {
    const levels = new Map<string, number>([[coordinator.name, 0]]);
    for (let pass = 0; pass < members.length; pass += 1) {
      relationships.forEach((edge) => {
        const level = levels.get(edge.from);
        if (level !== undefined && !levels.has(edge.to)) levels.set(edge.to, level + 1);
      });
    }
    let fallback = Math.max(0, ...levels.values()) + 1;
    others.forEach((member) => {
      if (!levels.has(member.name)) {
        levels.set(member.name, fallback);
        fallback += 1;
      }
    });
    const maxLevel = Math.max(0, ...levels.values());
    for (let level = 0; level <= maxLevel; level += 1) {
      const row = members.filter((member) => levels.get(member.name) === level);
      row.forEach((member, index) => positions.set(member.name, { x: ((index + 1) * width) / (row.length + 1), y: 58 + level * 112 }));
    }
    height = Math.max(260, 130 + maxLevel * 112);
  } else {
    positions.set(coordinator.name, { x: width / 2, y: 58 });
    others.forEach((member, index) => {
      const count = Math.min(3, others.length);
      positions.set(member.name, { x: (((index % 3) + 1) * width) / (count + 1), y: 178 + Math.floor(index / 3) * 112 });
    });
    height = Math.max(260, 130 + Math.ceil((members.length - 1) / 3) * 120);
  }
  return (
    <svg className="topology" viewBox={`0 0 ${width} ${height}`} role="img" aria-label={`Topology of ${agent.name}`}>
      <defs>
        <marker id="arrow" viewBox="0 0 10 10" refX="8" refY="5" markerWidth="5" markerHeight="5" orient="auto-start-reverse">
          <path d="M 0 0 L 10 5 L 0 10 z" />
        </marker>
      </defs>
      {relationships.map((edge) => {
        const from = positions.get(edge.from);
        const to = positions.get(edge.to);
        if (!from || !to) return null;
        return (
          <path
            key={`${edge.from}-${edge.to}-${edge.kind}`}
            className={`edge edge-${edge.kind}${activeEdge === `${edge.from}->${edge.to}` ? " edge-active" : ""}`}
            d={`M${from.x} ${from.y + 30} C${from.x} ${from.y + 80},${to.x} ${to.y - 65},${to.x} ${to.y - 30}`}
            markerEnd="url(#arrow)"
          />
        );
      })}
      {members.map((member) => {
        const point = positions.get(member.name);
        if (!point) return null;
        return (
          <g
            key={member.name}
            className={`topology-node${member.coordinator ? " coordinator" : ""}${activeMember === member.name ? " active" : ""}`}
            transform={`translate(${point.x - 73} ${point.y - 29})`}
          >
            <rect width="146" height="58" rx="9" />
            <circle cx="19" cy="19" r="6" />
            <text x="34" y="23">
              {member.name}
            </text>
            <text className="node-kind" x="13" y="45">
              {member.coordinator ? (hasFlow ? "Workflow" : "Coordinator") : hasFlow ? "Node" : "Member"}
            </text>
          </g>
        );
      })}
    </svg>
  );
}

function Protocols({
  agent,
  protocols,
  a2a,
  traceCount,
}: {
  agent?: AgentDetails;
  protocols?: UiCapabilities;
  a2a?: { card: A2aCard; path: string };
  traceCount: number;
}) {
  const services = agent?.services;
  const telemetry = traceCount > 0 ? "collecting" : services?.telemetryStatus ?? (services?.telemetry ? "configured" : "disabled");
  const entries = (protocols?.protocols ?? []).map((protocol) => (typeof protocol === "string" ? { protocol } : protocol));
  return (
    <div className="protocols">
      <h2 className="card-label">Runtime services</h2>
      <ul className="service-grid">
        <li data-on={telemetry !== "disabled"}>
          <strong>Telemetry</strong>
          <span>{telemetry === "collecting" ? "Session spans collected" : telemetry === "configured" ? "Collector ready" : "Exporter not configured"}</span>
        </li>
        <li data-on={!!services?.artifacts}>
          <strong>Artifacts</strong>
          <span>{services?.artifacts ? "Artifact store enabled" : "No artifact store"}</span>
        </li>
        <li data-on={!!services?.memory}>
          <strong>Memory</strong>
          <span>{services?.memory ? "Cross-session memory enabled" : "No memory service"}</span>
        </li>
        <li data-on={agent?.interactionMode === "realtime"}>
          <strong>Interaction</strong>
          <span>{agent?.interactionMode === "realtime" ? "Realtime audio and transcript" : "Request and response"}</span>
        </li>
      </ul>
      <h2 className="card-label">Agent-to-Agent</h2>
      {a2a ? (
        <a className="link" href={a2a.path} target="_blank" rel="noreferrer">
          {a2a.card.name || "A2A agent card"}
        </a>
      ) : (
        <p className="muted">No A2A discovery card is mounted on this server.</p>
      )}
      <h2 className="card-label">UI protocols</h2>
      <div className="data-list">
        {entries.map((entry) => (
          <details key={entry.protocol}>
            <summary>
              <strong className="mono">{entry.protocol}</strong>
              <span className="muted">{entry.versions?.join(", ") || "available"}</span>
            </summary>
            {entry.summary && <p>{entry.summary}</p>}
            {!!entry.features?.length && (
              <ul>
                {entry.features.map((feature) => (
                  <li key={feature} className="mono">
                    {feature}
                  </li>
                ))}
              </ul>
            )}
            {!!entry.limitations?.length && <p className="muted">Limitations: {entry.limitations.join("; ")}</p>}
          </details>
        ))}
      </div>
    </div>
  );
}

import { useCallback, useEffect, useMemo, useState } from "react";
import type { HomeClient } from "../client";
import type { GraphView } from "../generated/workflow";
import { GenuiProtocols } from "../genui/Protocols";
import type { WorkflowClient } from "../workflow/client";
import { workflowLabels } from "../workflow/labels";
import { runsOf } from "../workflow/route";
import { Runs } from "../workflow/Runs";
import { openWork, runFromEvents, runFromSpans } from "./run";
import { RunView, usePoll } from "./RunView";
import type { Edge } from "./RunView";
import { topologyActivity } from "./topologyActivity";
import type { A2aCard, AgentDetails, Session, TopologyMember, TraceSpan, UiCapabilities } from "./types";

type Tab = "run" | "protocols" | "state" | "runs";

/** The tabs' labels, in sentence case (spec 3.3 §8). */
const TAB_LABELS: Record<Tab, string> = {
  run: "Run",
  protocols: "Protocols",
  state: "State",
  runs: workflowLabels.tab,
};

/** How often a live run is read again. */
const LIVE_POLL_MS = 3000;

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
 * bundled showed and is worth keeping — the agent's graph, the tree and trace of a session's
 * run, the runtime services and UI protocols its server advertises, and a session's state — on
 * the server's own APIs. Developer panels may use technical words (§8).
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
  const [tab, setTab] = useState<Tab>(runsPlace ? "runs" : "run");
  const [problem, setProblem] = useState("");
  const [reads, setReads] = useState(0);

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
  }, [client, sessionId, app, user, reads]);

  const refresh = useCallback(() => setReads((count) => count + 1), []);
  // The trace where telemetry collects it, else the session's events.
  const run = useMemo(
    () => (traces.length > 0 ? runFromSpans(traces, Date.now(), openWork(session?.events ?? [])) : runFromEvents(session?.events ?? [])),
    // `reads` moves a live run's clock on even when nothing new arrived.
    [traces, session, reads],
  );
  usePoll(tab === "run" && run.live, LIVE_POLL_MS, refresh);
  const { members, edges } = useMemo(() => topologyOf(agent), [agent]);
  const { activeEdge } = topologyActivity(members, session?.events ?? [], run.live);
  const waitingNote = !sessionId
    ? "Open a thread to see its run here."
    : traces.length === 0 && !user
      ? "No trace for this session. Enter the user ID above to read its events, or turn on telemetry for the full trace."
      : "No steps yet. Send a message in this thread; its steps show here as they run.";

  const tabs: Tab[] = ["run", "protocols", "state", ...(workflow ? (["runs"] as const) : [])];
  const runsSession = runsPlace?.session ?? sessionId;
  return (
    <main className={`developer${tab === "run" ? " developer-wide" : ""}`}>
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
        {tab === "run" &&
          (agent ? (
            <RunView run={run} members={members} edges={edges} activeEdge={activeEdge} waitingNote={waitingNote} onRefresh={refresh} />
          ) : (
            <p className="muted">Select an agent to see its runs.</p>
          ))}
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
        {tab === "protocols" && (
          <div className="dev-protocols-split">
            <GenuiProtocols client={client} sessionId={sessionId} />
            <Protocols agent={agent} protocols={protocols} a2a={a2a} traceCount={traces.length} />
          </div>
        )}
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

/** The agent's members and the edges between them, from its topology or its children. */
function topologyOf(agent?: AgentDetails): { members: TopologyMember[]; edges: Edge[] } {
  if (!agent) return { members: [], edges: [] };
  const members: TopologyMember[] = agent.topology?.members ?? [
    { name: agent.name, description: agent.description, coordinator: true, capabilities: agent.capabilities },
    ...agent.children.map((child) => ({ ...child, coordinator: false })),
  ];
  const edges: Edge[] = agent.topology?.relationships ?? agent.children.map((child) => ({ from: agent.name, to: child.name, kind: "contains" as const }));
  return { members, edges };
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

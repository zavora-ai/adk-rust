import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { AdkClient, loadClient } from "./api";
import { MarkdownResponse } from "./MarkdownResponse";
import { topologyActivity } from "./topologyActivity";
import type { A2aCard, AgentDetails, EventPart, RuntimeEvent, Session, TopologyMember, TopologyRelationship, TraceSpan, UiCapabilities } from "./types";

type RunState = "connecting" | "ready" | "running" | "stopped" | "failed";
type InspectorTab = "topology" | "timeline" | "telemetry" | "state" | "artifacts" | "sessions" | "protocols";
type Theme = "system" | "light" | "dark";

const USER_ID_KEY = "adk.runtime.user";
const THEME_KEY = "adk.runtime.theme";

function eventText(event: RuntimeEvent): string {
  return (event.content?.parts ?? [])
    .map((part) => typeof part.text === "string" ? part.text : "")
    .filter(Boolean)
    .join("");
}

function eventTools(event: RuntimeEvent) {
  return (event.content?.parts ?? []).filter((part) => "name" in part || "functionResponse" in part || "function_response" in part);
}

function eventAudio(event: RuntimeEvent) {
  return (event.content?.parts ?? []).filter((part) => {
    const mimeType = part.mime_type ?? part.mimeType;
    return typeof mimeType === "string" && mimeType === "audio/wav" && part.data !== undefined;
  });
}

function audioUrl(part: EventPart): string | undefined {
  const mimeType = String(part.mime_type ?? part.mimeType ?? "audio/wav");
  let bytes: Uint8Array;
  if (Array.isArray(part.data)) bytes = Uint8Array.from(part.data as number[]);
  else if (typeof part.data === "string") {
    const decoded = atob(part.data);
    bytes = Uint8Array.from(decoded, (character) => character.charCodeAt(0));
  } else return undefined;
  const buffer = new ArrayBuffer(bytes.length);
  new Uint8Array(buffer).set(bytes);
  return URL.createObjectURL(new Blob([buffer], { type: mimeType }));
}

function AudioPlayback({ part }: { part: EventPart }) {
  const url = useMemo(() => audioUrl(part), [part]);
  useEffect(() => () => { if (url) URL.revokeObjectURL(url); }, [url]);
  return url ? <div className="audio-playback"><span>Realtime voice response</span><audio controls src={url} /></div> : null;
}

export function upsertEvent(events: RuntimeEvent[], incoming: RuntimeEvent): RuntimeEvent[] {
  const index = events.findIndex((event) => event.id === incoming.id);
  if (index < 0) return [...events, incoming];
  const current = events[index];
  const currentText = eventText(current);
  const nextText = eventText(incoming);
  const merged = currentText && !nextText
    ? { ...incoming, content: current.content }
    : incoming.partial && currentText && nextText
      ? { ...incoming, content: { ...incoming.content, parts: [{ text: currentText + nextText }] } }
      : incoming;
  return events.map((event, position) => position === index ? merged : event);
}

function Icon({ name }: { name: "sun" | "plus" | "send" | "stop" | "menu" | "close" | "refresh" | "attach" }) {
  const paths: Record<string, string> = {
    sun: "M12 3v2m0 14v2M3 12h2m14 0h2M5.6 5.6 7 7m10 10 1.4 1.4m0-12.8L17 7M7 17l-1.4 1.4M16 12a4 4 0 1 1-8 0 4 4 0 0 1 8 0Z",
    plus: "M12 5v14M5 12h14",
    send: "m4 4 16 8-16 8 3-8-3-8Zm3 8h13",
    stop: "M7 7h10v10H7z",
    menu: "M4 7h16M4 12h16M4 17h16",
    close: "m6 6 12 12M18 6 6 18",
    refresh: "M20 11a8 8 0 1 0-2.3 5.7M20 5v6h-6",
    attach: "m20.5 11.5-8.9 8.9a6 6 0 0 1-8.5-8.5l9.6-9.6a4 4 0 0 1 5.7 5.7l-9.6 9.6a2 2 0 0 1-2.8-2.8l8.9-8.9",
  };
  return <svg aria-hidden="true" viewBox="0 0 24 24"><path d={paths[name]} /></svg>;
}

function Status({ state }: { state: RunState }) {
  return <span className={`status status-${state}`}><i />{state}</span>;
}

function CapabilityChips({ agent }: { agent?: AgentDetails }) {
  const capabilities = agent?.capabilities;
  if (!capabilities) return null;
  const labels = [
    [agent.interactionMode === "realtime", "Realtime voice"],
    [capabilities.runtimeTools, "Tools"],
    [capabilities.handoff, "Handoff"],
    [capabilities.sharedState, "Shared state"],
    [capabilities.checkpointResume, "Resume"],
    [capabilities.invocationMetadata, "Metadata"],
  ] as const;
  return <div className="capabilities">{labels.filter(([enabled]) => enabled).map(([, label]) => <span key={label}>{label}</span>)}</div>;
}

function Message({ event }: { event: RuntimeEvent }) {
  const text = eventText(event);
  const tools = eventTools(event);
  const audio = eventAudio(event);
  const role = event.content?.role ?? (event.author === "user" ? "user" : "model");
  const isUser = role === "user" || event.author === "user";
  const transfer = event.actions?.transfer_to_agent;
  const confirmation = event.actions?.tool_confirmation;
  return <article className={`message ${isUser ? "message-user" : "message-agent"}`}>
    <header><span className="avatar">{isUser ? "U" : (event.author?.[0] ?? "A").toUpperCase()}</span><strong>{isUser ? "You" : (event.author || "Agent")}</strong>{event.partial && <span className="streaming">streaming</span>}</header>
    {text && <div className="message-text">{isUser ? text : <MarkdownResponse>{text}</MarkdownResponse>}</div>}
    {tools.map((tool, index) => <details className="tool-call" key={`${event.id}-${index}`}>
      <summary>{"functionResponse" in tool || "function_response" in tool ? "Tool result" : `Tool · ${String(tool.name ?? "call")}`}</summary>
      <pre>{JSON.stringify(tool, null, 2)}</pre>
    </details>)}
    {audio.map((part, index) => <AudioPlayback part={part} key={`${event.id}-audio-${index}`} />)}
    {transfer && <div className="handoff-note">Control handed off to <strong>{transfer}</strong></div>}
    {!!confirmation && <details className="confirmation-note"><summary>Approval required before tool execution</summary><pre>{JSON.stringify(confirmation, null, 2)}</pre></details>}
    {event.actions?.escalate && <div className="event-error">This run escalated for human attention.</div>}
    {event.error_message && <div className="event-error">{event.error_message}</div>}
  </article>;
}

function Topology({ agent, events, running }: { agent?: AgentDetails; events: RuntimeEvent[]; running: boolean }) {
  if (!agent) return <Empty label="Select an agent to inspect its runtime topology." />;
  const members: TopologyMember[] = agent.topology?.members ?? [
    { name: agent.name, description: agent.description, coordinator: true, capabilities: agent.capabilities },
    ...agent.children.map((child) => ({ ...child, coordinator: false })),
  ];
  const relationships: Array<TopologyRelationship | { from: string; to: string; kind: "contains" }> = agent.topology?.relationships
    ?? agent.children.map((child) => ({ from: agent.name, to: child.name, kind: "contains" as const }));
  const width = 560;
  const coordinator = members.find((member) => member.coordinator) ?? members[0];
  const { activeMember, activeEdge } = topologyActivity(members, events, running);
  const others = members.filter((member) => member.name !== coordinator.name);
  const positions = new Map<string, { x: number; y: number }>();
  const hasFlow = relationships.some((edge) => edge.kind === "flow");
  let height: number;
  if (hasFlow) {
    const levels = new Map<string, number>([[coordinator.name, 0]]);
    for (let pass = 0; pass < members.length; pass += 1) {
      relationships.forEach((edge) => {
        const sourceLevel = levels.get(edge.from);
        if (sourceLevel !== undefined && !levels.has(edge.to)) levels.set(edge.to, sourceLevel + 1);
      });
    }
    let fallbackLevel = Math.max(0, ...levels.values()) + 1;
    others.forEach((member) => {
      if (!levels.has(member.name)) { levels.set(member.name, fallbackLevel); fallbackLevel += 1; }
    });
    const maxLevel = Math.max(0, ...levels.values());
    for (let level = 0; level <= maxLevel; level += 1) {
      const row = members.filter((member) => levels.get(member.name) === level);
      row.forEach((member, index) => positions.set(member.name, {
        x: ((index + 1) * width) / (row.length + 1),
        y: 58 + level * 112,
      }));
    }
    height = Math.max(260, 130 + maxLevel * 112);
  } else {
    positions.set(coordinator.name, { x: width / 2, y: 58 });
    others.forEach((member, index) => {
      const count = Math.min(3, others.length);
      const row = Math.floor(index / 3);
      const column = index % 3;
      positions.set(member.name, { x: ((column + 1) * width) / (count + 1), y: 178 + row * 112 });
    });
    height = Math.max(260, 130 + Math.ceil((members.length - 1) / 3) * 120);
  }
  return <div className="topology-wrap">
    <div className="legend">{hasFlow ? <span><i className="line flow" />Workflow flow</span> : agent.topology ? <><span><i className="line delegate" />Delegate & return</span><span><i className="line handoff" />Handoff control</span></> : <span><i className="line contains" />Managed child</span>}</div>
    <svg className="topology" viewBox={`0 0 ${width} ${height}`} role="img" aria-label={`Topology for ${agent.name}`}>
      <defs><marker id="arrow" viewBox="0 0 10 10" refX="8" refY="5" markerWidth="5" markerHeight="5" orient="auto-start-reverse"><path d="M 0 0 L 10 5 L 0 10 z" /></marker></defs>
      {relationships.map((edge) => {
        const from = positions.get(edge.from); const to = positions.get(edge.to);
        if (!from || !to) return null;
        const edgeKey = `${edge.from}->${edge.to}`;
        return <path key={`${edge.from}-${edge.to}-${edge.kind}`} className={`edge edge-${edge.kind}${activeEdge === edgeKey ? " edge-active" : ""}`} d={`M${from.x} ${from.y + 30} C${from.x} ${from.y + 80},${to.x} ${to.y - 65},${to.x} ${to.y - 30}`} markerEnd="url(#arrow)" />;
      })}
      {members.map((member) => {
        const point = positions.get(member.name)!;
        return <g key={member.name} className={`topology-node${member.coordinator ? " coordinator" : ""}${activeMember === member.name ? " active" : ""}`} transform={`translate(${point.x - 73} ${point.y - 29})`}>
          <rect width="146" height="58" rx="9" />
          <circle className="node-pulse" cx="19" cy="19" r="8" />
          <circle cx="19" cy="19" r="8" />
          <text x="34" y="23">{member.name}</text>
          <text className="node-kind" x="13" y="45">{member.coordinator ? (hasFlow ? "WORKFLOW" : "COORDINATOR") : (hasFlow ? "NODE" : "MEMBER")}</text>
        </g>;
      })}
    </svg>
  </div>;
}

function Empty({ label }: { label: string }) {
  return <div className="empty"><div className="empty-mark">◇</div><p>{label}</p></div>;
}

function ProtocolsPanel({ agent, protocols, a2a, traceCount }: { agent?: AgentDetails; protocols?: UiCapabilities; a2a?: { card: A2aCard; path: string }; traceCount: number }) {
  const services = agent?.services;
  const telemetryStatus = traceCount > 0 ? "collecting" : (services?.telemetryStatus ?? (services?.telemetry ? "configured" : "disabled"));
  const telemetryLabel = telemetryStatus === "collecting" ? "Session spans collected" : telemetryStatus === "configured" ? "Collector ready" : "Exporter not configured";
  const entries = (protocols?.protocols ?? []).map((protocol) => typeof protocol === "string" ? { protocol } : protocol);
  return <div className="protocol-panel">
    <div className="subhead">Runtime services</div>
    <div className="service-grid">
      <div className={telemetryStatus !== "disabled" ? "service-card enabled" : "service-card"}><strong>Telemetry</strong><span>{telemetryLabel}</span></div>
      <div className={services?.artifacts ? "service-card enabled" : "service-card"}><strong>Artifacts</strong><span>{services?.artifacts ? "Artifact store enabled" : "No artifact store"}</span></div>
      <div className={services?.memory ? "service-card enabled" : "service-card"}><strong>Memory</strong><span>{services?.memory ? "Cross-session memory enabled" : "No memory service"}</span></div>
      <div className={agent?.interactionMode === "realtime" ? "service-card enabled" : "service-card"}><strong>Interaction</strong><span>{agent?.interactionMode === "realtime" ? "Realtime audio + transcript" : "Request / response"}</span></div>
    </div>
    <div className="subhead">Agent-to-Agent</div>
    {a2a ? <a className="protocol-card protocol-link" href={a2a.path} target="_blank" rel="noreferrer"><strong>{a2a.card.name || "A2A agent card"}</strong><span>Discovery enabled · open agent card ↗</span></a> : <p className="muted-copy protocol-empty">No A2A discovery card is mounted on this server.</p>}
    <div className="subhead">UI protocols</div>
    <div className="protocol-list">{entries.map((entry) => <details className="protocol-card" key={entry.protocol}>
      <summary><strong>{entry.protocol.replaceAll("_", " ")}</strong><span>{entry.versions?.join(", ") || "available"}</span></summary>
      {entry.summary && <p>{entry.summary}</p>}
      {!!entry.features?.length && <ul>{entry.features.map((feature) => <li key={feature}>{feature}</li>)}</ul>}
      {!!entry.limitations?.length && <p className="protocol-limit">Limitations: {entry.limitations.join("; ")}</p>}
    </details>)}</div>
  </div>;
}

export function App() {
  const [client, setClient] = useState<AdkClient>();
  const [agents, setAgents] = useState<string[]>([]);
  const [agentName, setAgentName] = useState("");
  const [agent, setAgent] = useState<AgentDetails>();
  const [protocols, setProtocols] = useState<UiCapabilities>();
  const [a2a, setA2a] = useState<{ card: A2aCard; path: string }>();
  const [userId, setUserId] = useState(() => localStorage.getItem(USER_ID_KEY) || "local-user");
  const [session, setSession] = useState<Session>();
  const [sessions, setSessions] = useState<Session[]>([]);
  const [events, setEvents] = useState<RuntimeEvent[]>([]);
  const [artifacts, setArtifacts] = useState<string[]>([]);
  const [traces, setTraces] = useState<TraceSpan[]>([]);
  const [message, setMessage] = useState("");
  const [attachments, setAttachments] = useState<File[]>([]);
  const [runState, setRunState] = useState<RunState>("connecting");
  const [error, setError] = useState("");
  const [tab, setTab] = useState<InspectorTab>("topology");
  const [theme, setTheme] = useState<Theme>(() => (localStorage.getItem(THEME_KEY) as Theme) || "system");
  const [inspectorOpen, setInspectorOpen] = useState(false);
  const abortRef = useRef<AbortController | undefined>(undefined);
  const transcriptRef = useRef<HTMLDivElement>(null);

  useEffect(() => {
    document.documentElement.dataset.theme = theme === "system" ? "" : theme;
    localStorage.setItem(THEME_KEY, theme);
  }, [theme]);

  useEffect(() => {
    loadClient().then(async (loaded) => {
      setClient(loaded);
      try {
        const [names, capabilities, card] = await Promise.all([loaded.listAgents(), loaded.capabilities(), loaded.a2aCard().catch(() => undefined)]);
        const sortedNames = [...names].sort();
        setAgents(sortedNames); setProtocols(capabilities); setA2a(card); setAgentName(sortedNames[0] ?? ""); setRunState("ready");
      } catch (cause) { setRunState("failed"); setError(cause instanceof Error ? cause.message : String(cause)); }
    });
  }, []);

  useEffect(() => {
    if (!client || !agentName) return;
    setError("");
    Promise.all([client.agent(agentName), client.listSessions(agentName, userId)])
      .then(([details, available]) => { setAgent(details); setSessions(available.sort((a, b) => b.lastUpdateTime - a.lastUpdateTime)); })
      .catch((cause) => setError(cause instanceof Error ? cause.message : String(cause)));
  }, [client, agentName, userId]);

  useEffect(() => { transcriptRef.current?.scrollTo({ top: transcriptRef.current.scrollHeight, behavior: "smooth" }); }, [events]);

  const refreshSession = useCallback(async (target = session) => {
    if (!client || !target) return;
    const fresh = await client.getSession(agentName, userId, target.id);
    setSession(fresh); setEvents(fresh.events);
    const [artifactNames, sessionTraces, availableSessions] = await Promise.all([
      client.listArtifacts(agentName, userId, target.id),
      client.listTraces(target.id),
      client.listSessions(agentName, userId),
    ]);
    setArtifacts(artifactNames);
    setTraces(sessionTraces);
    setSessions(availableSessions);
  }, [client, session, agentName, userId]);

  useEffect(() => {
    if (!client || !agentName) return;
    let cancelled = false;
    const poll = async () => {
      try {
        const available = (await client.listSessions(agentName, userId)).sort((a, b) => b.lastUpdateTime - a.lastUpdateTime);
        if (cancelled) return;
        setSessions(available);
        const active = session ? available.find((item) => item.id === session.id) : available[0];
        if (!active || runState === "running") return;
        const [fresh, artifactNames, sessionTraces] = await Promise.all([
          client.getSession(agentName, userId, active.id),
          client.listArtifacts(agentName, userId, active.id),
          client.listTraces(active.id),
        ]);
        if (cancelled) return;
        setSession(fresh); setEvents(fresh.events); setArtifacts(artifactNames); setTraces(sessionTraces);
      } catch {
        // Polling supplements explicit refresh. Transient failures remain silent.
      }
    };
    const timer = window.setInterval(() => void poll(), 4_000);
    void poll();
    return () => { cancelled = true; window.clearInterval(timer); };
  }, [client, agentName, userId, session?.id, runState]);

  const newSession = useCallback(async () => {
    if (!client || !agentName) return;
    setError("");
    try {
      const created = await client.createSession(agentName, userId);
      setSession(created); setEvents([]); setArtifacts([]); setTraces([]); setSessions((current) => [created, ...current]); setRunState("ready");
    } catch (cause) { setError(cause instanceof Error ? cause.message : String(cause)); }
  }, [client, agentName, userId]);

  const send = useCallback(async () => {
    const text = message.trim();
    if (!client || (!text && !attachments.length) || runState === "running") return;
    let active = session;
    try {
      if (!active) { active = await client.createSession(agentName, userId); setSession(active); }
      const optimisticText = text || `Attached: ${attachments.map((file) => file.name).join(", ")}`;
      const userEvent: RuntimeEvent = { id: `local-${crypto.randomUUID()}`, author: "user", content: { role: "user", parts: [{ text: optimisticText }] } };
      setEvents((current) => [...current, userEvent]); setMessage(""); setError(""); setRunState("running");
      const abort = new AbortController(); abortRef.current = abort;
      const selectedAttachments = attachments;
      setAttachments([]);
      await client.run(agentName, userId, active.id, text, selectedAttachments, abort.signal, (event) => setEvents((current) => upsertEvent(current, event)));
      setRunState("ready");
      try {
        await refreshSession(active);
      } catch (cause) {
        setError(`The run completed, but the session could not be refreshed: ${cause instanceof Error ? cause.message : String(cause)}`);
      }
    } catch (cause) {
      if (cause instanceof DOMException && cause.name === "AbortError") setRunState("stopped");
      else { setRunState("failed"); setError(cause instanceof Error ? cause.message : String(cause)); }
    } finally { abortRef.current = undefined; }
  }, [client, message, attachments, runState, session, agentName, userId, refreshSession]);

  const stateEntries = Object.entries(session?.state ?? {});
  const protocolNames = (protocols?.protocols ?? protocols?.supportedProtocols ?? []).map((protocol) => typeof protocol === "string" ? protocol : protocol.protocol);
  const visibleEvents = events.filter((event) => eventText(event) || eventTools(event).length || eventAudio(event).length || event.actions?.transfer_to_agent || event.actions?.tool_confirmation || event.actions?.escalate || event.error_message);
  const isWorkflow = agent?.topology?.relationships.some((relationship) => relationship.kind === "flow") ?? false;
  const telemetryStatus = traces.length > 0 ? "collecting" : (agent?.services.telemetryStatus ?? (agent?.services.telemetry ? "configured" : "disabled"));
  const toggleTheme = () => setTheme((current) => current === "system" ? "light" : current === "light" ? "dark" : "system");

  return <div className="app-shell">
    <header className="topbar">
      <button className="icon-button mobile-only" aria-label="Open inspector" onClick={() => setInspectorOpen(true)}><Icon name="menu" /></button>
      <a className="brand" href="https://adk-rust.com" target="_blank" rel="noreferrer" aria-label="Open the ADK-Rust website"><span className="brand-mark">A</span><span>ADK <b>Runtime</b></span></a>
      <span className="top-divider" />
      <label className="agent-picker"><span>Agent</span><select value={agentName} onChange={(event) => { setAgentName(event.target.value); setSession(undefined); setEvents([]); }}>{agents.map((name) => <option key={name}>{name}</option>)}</select></label>
      <div className="top-grow" />
      <Status state={runState} />
      <button className="icon-button" aria-label={`Theme: ${theme}. Change theme`} title={`Theme: ${theme}`} onClick={toggleTheme}><Icon name="sun" /></button>
      <button className="primary-button" onClick={newSession} disabled={!client || !agentName}><Icon name="plus" />New session</button>
    </header>

    {error && <div className="error-banner" role="alert"><strong>Runtime error</strong><span>{error}</span><button onClick={() => setError("")} aria-label="Dismiss error"><Icon name="close" /></button></div>}

    <main className="workspace">
      <aside className="agent-rail">
        <div className="panel-heading"><span>Runtime target</span><span className="kind-chip">{agent?.kind ?? "agent"}</span></div>
        <div className="agent-summary"><div className="agent-avatar">{agentName.slice(0, 2).toUpperCase()}</div><div><strong>{agentName || "No agent"}</strong><p>{agent?.description || "Choose an executable agent root."}</p></div></div>
        <CapabilityChips agent={agent} />
        <div className="panel-heading section-heading"><span>{isWorkflow ? "Workflow nodes" : agent?.topology ? "Team roster" : "Agent hierarchy"}</span><span>{agent?.topology?.members.length ?? agent?.children.length ?? 0}</span></div>
        <div className="roster">
          {(agent?.topology?.members ?? agent?.children ?? []).map((member) => <div className="roster-item" key={member.name}><span className="member-dot" /><div><strong>{member.name}</strong><p>{"coordinator" in member && member.coordinator ? "Coordinator" : (member.description || "Member agent")}</p></div></div>)}
          {!agent?.topology?.members.length && !agent?.children.length && <p className="muted-copy">Leaf agent · no child runtime targets</p>}
        </div>
        <div className="rail-footer"><label>User ID<input value={userId} onChange={(event) => { setUserId(event.target.value); localStorage.setItem(USER_ID_KEY, event.target.value); }} /></label><div className="protocols">{protocolNames.map((name) => <span key={name}>{name.replaceAll("_", " ")}</span>)}</div></div>
      </aside>

      <section className="conversation" aria-label="Agent conversation">
        <header className="conversation-header"><div><span className="eyebrow">Interactive run</span><h1>{session ? `Session ${session.id.slice(0, 8)}` : "Start a conversation"}</h1></div>{session && <button className="quiet-button" onClick={() => refreshSession()}><Icon name="refresh" />Refresh</button>}</header>
        <div className="transcript" ref={transcriptRef} aria-live="polite">
          {!visibleEvents.length ? <Empty label="Send a message to inspect responses, tools, handoffs, state changes, and artifacts as they happen." /> : visibleEvents.map((event) => <Message key={event.id} event={event} />)}
        </div>
        <div className="composer-wrap">
          {!!attachments.length && <div className="attachment-chips">{attachments.map((file, index) => <button key={`${file.name}-${index}`} onClick={() => setAttachments((current) => current.filter((_, position) => position !== index))} title="Remove attachment"><span>◆</span>{file.name}<b>×</b></button>)}</div>}
          <div className="composer">
            <label className="attach-button" title="Attach files"><span className="sr-only">Attach files</span><Icon name="attach" /><input type="file" multiple onChange={(event) => {
              const chosen = Array.from(event.target.files ?? []);
              const total = [...attachments, ...chosen].reduce((sum, file) => sum + file.size, 0);
              if (total > 10 * 1024 * 1024) setError("Attachments must total 10 MB or less.");
              else setAttachments((current) => [...current, ...chosen]);
              event.target.value = "";
            }} /></label>
            <textarea value={message} onChange={(event) => setMessage(event.target.value)} onKeyDown={(event) => { if (event.key === "Enter" && !event.shiftKey) { event.preventDefault(); void send(); } }} placeholder={`Message ${agentName || "agent"}…`} aria-label="Message agent" rows={2} />
            {runState === "running" ? <button className="send-button stop-button" aria-label="Stop response stream" onClick={() => abortRef.current?.abort()}><Icon name="stop" /></button> : <button className="send-button" aria-label="Send message" disabled={(!message.trim() && !attachments.length) || !client} onClick={() => void send()}><Icon name="send" /></button>}
          </div>
          <div className="composer-hint"><span>Enter to send · up to 10 MB of attachments</span><span>{session ? session.id : "A session will be created automatically"}</span></div>
        </div>
      </section>

      <aside className={`inspector ${inspectorOpen ? "inspector-open" : ""}`}>
        <div className="inspector-mobile-head"><strong>Runtime inspector</strong><button className="icon-button" aria-label="Close inspector" onClick={() => setInspectorOpen(false)}><Icon name="close" /></button></div>
        <nav className="tabs" aria-label="Runtime inspector">
          {(["topology", "timeline", "telemetry", "state", "artifacts", "sessions", "protocols"] as InspectorTab[]).map((name) => <button key={name} aria-selected={tab === name} onClick={() => setTab(name)}>{name}<small>{name === "timeline" ? events.length : name === "telemetry" ? traces.length : name === "state" ? stateEntries.length : name === "artifacts" ? artifacts.length : name === "sessions" ? sessions.length : undefined}</small></button>)}
        </nav>
        <div className="inspector-content">
          {tab === "topology" && <Topology agent={agent} events={events} running={runState === "running"} />}
          {tab === "timeline" && (!events.length ? <Empty label="Runtime events will appear here in emission order." /> : <div><div className="subhead">Events <span>{events.length}</span></div><ol className="timeline">{events.map((event, index) => <li key={`${event.id}-${index}`}><span className="timeline-dot" /><div><strong>{event.author || event.content?.role || "runtime"}</strong><time>{event.timestamp ? new Date(event.timestamp).toLocaleTimeString() : `event ${index + 1}`}</time><p>{event.actions?.transfer_to_agent ? `Handoff → ${event.actions.transfer_to_agent}` : eventTools(event).length ? "Tool activity" : eventAudio(event).length ? "Realtime voice playback" : eventText(event).slice(0, 100) || "State/action event"}</p></div></li>)}</ol></div>)}
          {tab === "telemetry" && (!traces.length ? <Empty label={telemetryStatus === "configured" ? "Telemetry is configured. Run this session to verify span collection." : telemetryStatus === "collecting" ? "Telemetry is collecting, but this session has no retained spans." : "Telemetry is not configured on this server."} /> : <div><div className="telemetry-summary"><strong>{traces.length}</strong><span>session spans</span><small>{new Set(traces.map((trace) => trace.trace_id).filter(Boolean)).size} traces</small></div><div className="trace-list">{traces.map((trace) => <details key={trace.span_id}><summary><strong>{trace.name}</strong><span>{trace.end_time > trace.start_time ? `${((trace.end_time - trace.start_time) / 1_000_000).toFixed(1)} ms` : "active"}</span></summary><pre>{JSON.stringify(trace.attributes, null, 2)}</pre></details>)}</div></div>)}
          {tab === "state" && (!stateEntries.length ? <Empty label="Session and shared team state will appear after a run." /> : <div className="data-list">{stateEntries.map(([key, value]) => <details key={key}><summary>{key}</summary><pre>{JSON.stringify(value, null, 2)}</pre></details>)}</div>)}
          {tab === "artifacts" && (!artifacts.length ? <Empty label="Artifacts saved by the agent will appear here." /> : <div className="artifact-list">{artifacts.map((name) => <a key={name} href={`${client?.baseUrl}/sessions/${encodeURIComponent(agentName)}/${encodeURIComponent(userId)}/${encodeURIComponent(session!.id)}/artifacts/${encodeURIComponent(name)}`} target="_blank" rel="noreferrer"><span>◆</span>{name}</a>)}</div>)}
          {tab === "sessions" && (!sessions.length ? <Empty label="No sessions for this agent and user yet." /> : <div className="session-list">{sessions.map((item) => <button key={item.id} className={session?.id === item.id ? "active" : ""} onClick={() => { setSession(item); setEvents(item.events); setInspectorOpen(false); void Promise.all([client?.listArtifacts(agentName, userId, item.id), client?.listTraces(item.id)]).then(([names, spans]) => { setArtifacts(names ?? []); setTraces(spans ?? []); }); }}><strong>{item.id.slice(0, 12)}</strong><span>{new Date(item.lastUpdateTime * 1000).toLocaleString()}</span><small>{item.events.length} events</small></button>)}</div>)}
          {tab === "protocols" && <ProtocolsPanel agent={agent} protocols={protocols} a2a={a2a} traceCount={traces.length} />}
        </div>
      </aside>
      {inspectorOpen && <button className="scrim" aria-label="Close inspector" onClick={() => setInspectorOpen(false)} />}
    </main>
  </div>;
}

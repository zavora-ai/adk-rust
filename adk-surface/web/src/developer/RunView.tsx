import { useEffect, useMemo, useRef, useState } from "react";
import type { KeyboardEvent } from "react";
import { visible } from "../safe";
import "./developer.css";
import { duration, memberStates, shownSteps, tickText, ticks, tokensText } from "./run";
import type { MemberState, Run, Step, StepKind } from "./run";
import type { TopologyMember } from "./types";

export type Edge = { from: string; to: string; kind: "flow" | "delegate" | "handoff" | "contains" };

type Selection = { type: "step"; id: string } | { type: "agent"; name: string };

const KIND_LABEL: Record<StepKind, string> = {
  agent: "Agent",
  handoff: "Handoff",
  model: "Model call",
  tool: "Tool call",
  held: "Held for you",
  flow: "Flow edge",
  person: "Person",
  step: "Step",
};

const STATE_LABEL: Record<MemberState, string> = {
  idle: "Idle",
  active: "Working",
  held: "Needs you",
  done: "Done",
};

/**
 * The developer view of one run (spec 3.3 §12 developer track): the graph of who handed work to
 * whom, the tree of every step, and the trace of those steps on one clock, with an inspector for
 * the step or agent picked in any of them. Developer panels may use technical words (§8).
 */
export function RunView({
  run,
  members,
  edges,
  activeEdge,
  waitingNote,
  onRefresh,
}: {
  run: Run;
  members: TopologyMember[];
  edges: Edge[];
  activeEdge?: string;
  /** Why the run is empty, when it is. */
  waitingNote: string;
  onRefresh: () => void;
}) {
  const [selection, setSelection] = useState<Selection | undefined>();
  const [folded, setFolded] = useState<ReadonlySet<string>>(new Set());
  const states = useMemo(() => memberStates(run, members), [run, members]);
  const shown = useMemo(() => shownSteps(run.steps, folded), [run.steps, folded]);
  const parents = useMemo(() => new Set(run.steps.map((step) => step.parent).filter(Boolean) as string[]), [run.steps]);

  // A run opens on what needs the person, else on its latest step.
  const fallback = run.steps.find((step) => step.kind === "held" && step.end === undefined) ?? run.steps[run.steps.length - 1];
  const picked: Selection | undefined =
    selection?.type === "step" && !run.steps.some((step) => step.id === selection.id) ? undefined : selection;
  const current = picked ?? (fallback ? { type: "step" as const, id: fallback.id } : undefined);
  const step = current?.type === "step" ? run.steps.find((candidate) => candidate.id === current.id) : undefined;
  const agentName = current?.type === "agent" ? current.name : step?.agent;

  const toggle = (id: string) =>
    setFolded((previous) => {
      const next = new Set(previous);
      if (next.has(id)) next.delete(id);
      else next.add(id);
      return next;
    });

  const held = run.steps.some((candidate) => candidate.kind === "held" && candidate.end === undefined);
  return (
    <div className="dv">
      <div className="dv-runbar" role="group" aria-label="This run">
        <span className={`dv-chip ${held ? "dv-chip-held" : run.live ? "dv-chip-live" : ""}`}>
          {(held || run.live) && <span className="dv-pulse" aria-hidden="true" />}
          {held ? "Held for you" : run.live ? "Live" : run.steps.length ? "Finished" : "No run yet"}
        </span>
        {run.steps.length > 0 && (
          <>
            <span className="dv-chip">{duration(run.total)}</span>
            <span className="dv-chip">
              {run.agents.length} {run.agents.length === 1 ? "agent" : "agents"} · {run.steps.length} {run.steps.length === 1 ? "step" : "steps"}
            </span>
            {run.tokens.input + run.tokens.output > 0 && (
              <span className="dv-chip">
                {tokensText(run.tokens.input)} in · {tokensText(run.tokens.output)} out
              </span>
            )}
            <span className="dv-chip dv-chip-quiet">{run.source === "trace" ? "from the trace" : "from session events"}</span>
          </>
        )}
        <button type="button" className="button button-secondary dv-refresh" onClick={onRefresh}>
          Refresh
        </button>
      </div>

      <div className="dv-grid">
        <section className="dv-pane dv-tree-pane" aria-labelledby="dv-tree-title">
          <header className="dv-pane-head">
            <h2 id="dv-tree-title">Tree</h2>
            <span className="muted">agents → steps</span>
          </header>
          {run.steps.length === 0 ? (
            <p className="dv-empty">{waitingNote}</p>
          ) : (
            <Tree
              steps={shown}
              parents={parents}
              folded={folded}
              selected={current?.type === "step" ? current.id : undefined}
              onSelect={(id) => setSelection({ type: "step", id })}
              onToggle={toggle}
            />
          )}
          <Legend />
        </section>

        <div className="dv-center">
          <section className="dv-pane" aria-labelledby="dv-graph-title">
            <header className="dv-pane-head">
              <h2 id="dv-graph-title">Graph</h2>
              <span className="muted">who hands work to whom</span>
            </header>
            <Graph
              members={members}
              edges={edges}
              states={states}
              activeEdge={activeEdge}
              selected={agentName}
              onSelect={(name) => {
                const top = run.steps.find((candidate) => candidate.agent === name && (candidate.kind === "agent" || candidate.kind === "handoff"));
                setSelection(top ? { type: "step", id: top.id } : { type: "agent", name });
              }}
            />
          </section>
          <section className="dv-pane" aria-labelledby="dv-trace-title">
            <header className="dv-pane-head">
              <h2 id="dv-trace-title">Trace</h2>
              <span className="muted">every step on one clock{run.steps.length > 0 ? ` · ${duration(run.total)}` : ""}</span>
            </header>
            {run.steps.length === 0 ? (
              <p className="dv-empty">The trace fills in as the run's steps finish.</p>
            ) : (
              <Trace run={run} steps={shown} selected={current?.type === "step" ? current.id : undefined} onSelect={(id) => setSelection({ type: "step", id })} />
            )}
          </section>
        </div>

        <aside className="dv-pane dv-inspector" aria-labelledby="dv-inspect-title">
          {step ? (
            <StepInspector step={step} run={run} />
          ) : agentName ? (
            <AgentInspector member={members.find((member) => member.name === agentName)} name={agentName} state={states.get(agentName)} />
          ) : (
            <>
              <h2 id="dv-inspect-title" className="dv-inspect-title">
                Inspector
              </h2>
              <p className="dv-empty">Pick an agent in the graph, or a step in the tree or the trace.</p>
            </>
          )}
        </aside>
      </div>
    </div>
  );
}

function Legend() {
  return (
    <ul className="dv-legend" aria-label="Kinds of step">
      {(["model", "tool", "handoff", "held"] as const).map((kind) => (
        <li key={kind}>
          <span className={`dv-mark dv-kind-${kind}`} aria-hidden="true" />
          {kind === "handoff" ? "Agent handoff" : KIND_LABEL[kind]}
        </li>
      ))}
    </ul>
  );
}

function Tree({
  steps,
  parents,
  folded,
  selected,
  onSelect,
  onToggle,
}: {
  steps: Step[];
  parents: ReadonlySet<string>;
  folded: ReadonlySet<string>;
  selected?: string;
  onSelect: (id: string) => void;
  onToggle: (id: string) => void;
}) {
  const items = useRef(new Map<string, HTMLLIElement>());
  const focusable = selected && steps.some((step) => step.id === selected) ? selected : steps[0]?.id;
  const move = (id: string) => {
    onSelect(id);
    items.current.get(id)?.focus();
  };
  const onKey = (event: KeyboardEvent, step: Step, index: number) => {
    const open = parents.has(step.id) && !folded.has(step.id);
    if (event.key === "ArrowDown" && steps[index + 1]) move(steps[index + 1].id);
    else if (event.key === "ArrowUp" && steps[index - 1]) move(steps[index - 1].id);
    else if (event.key === "Home" && steps[0]) move(steps[0].id);
    else if (event.key === "End" && steps.length) move(steps[steps.length - 1].id);
    else if (event.key === "ArrowRight" && parents.has(step.id)) {
      if (!open) onToggle(step.id);
      else if (steps[index + 1]) move(steps[index + 1].id);
    } else if (event.key === "ArrowLeft") {
      if (open) onToggle(step.id);
      else if (step.parent) move(step.parent);
    } else if (event.key === "Enter" || event.key === " ") onSelect(step.id);
    else return;
    event.preventDefault();
  };
  return (
    <ul className="dv-tree" role="tree" aria-label="Steps of this run">
      {steps.map((step, index) => {
        const isParent = parents.has(step.id);
        return (
          <li
            key={step.id}
            ref={(node) => {
              if (node) items.current.set(step.id, node);
              else items.current.delete(step.id);
            }}
            role="treeitem"
            aria-level={step.depth + 1}
            aria-selected={step.id === selected}
            aria-expanded={isParent ? !folded.has(step.id) : undefined}
            tabIndex={step.id === focusable ? 0 : -1}
            className="dv-tree-item"
            onClick={() => onSelect(step.id)}
            onKeyDown={(event) => onKey(event, step, index)}
            style={{ paddingInlineStart: `${6 + step.depth * 14}px` }}
          >
            <span
              className="dv-fold"
              aria-hidden="true"
              data-open={isParent ? !folded.has(step.id) : undefined}
              onClick={(event) => {
                if (!isParent) return;
                event.stopPropagation();
                onToggle(step.id);
              }}
            />
            <span className={`dv-mark dv-kind-${step.kind}`} aria-hidden="true" />
            <span className="dv-tree-label">{step.label}</span>
            <span className={`dv-time${step.kind === "held" && step.end === undefined ? " dv-time-held" : ""}`}>
              {step.end === undefined ? "running" : duration(step.end - step.start)}
            </span>
          </li>
        );
      })}
    </ul>
  );
}

function Trace({ run, steps, selected, onSelect }: { run: Run; steps: Step[]; selected?: string; onSelect: (id: string) => void }) {
  const marks = ticks(run.total);
  const at = (ms: number) => `${Math.min(100, (ms / run.total) * 100).toFixed(3)}%`;
  return (
    <div className="dv-trace">
      <div className="dv-axis" aria-hidden="true">
        {marks.map((mark) => (
          <span key={mark} style={{ left: at(mark) }}>
            {tickText(mark)}
          </span>
        ))}
      </div>
      <ol className="dv-rows" aria-label="Steps on a timeline">
        {steps.map((step) => {
          const end = step.end ?? run.total;
          const width = Math.max(0, ((end - step.start) / run.total) * 100);
          return (
            <li key={step.id}>
              <button
                type="button"
                className="dv-row"
                aria-pressed={step.id === selected}
                aria-label={`${step.label}, ${KIND_LABEL[step.kind]}, starts at ${duration(step.start)}, ${step.end === undefined ? "still running" : duration(end - step.start)}`}
                onClick={() => onSelect(step.id)}
              >
                <span className="dv-row-label" style={{ paddingInlineStart: `${step.depth * 12}px` }}>
                  {step.label}
                </span>
                <span className="dv-track">
                  {marks.map((mark) => (
                    <span key={mark} className="dv-gridline" style={{ left: at(mark) }} />
                  ))}
                  <span
                    className={`dv-span dv-kind-${step.kind}${step.end === undefined ? " dv-span-live" : ""}`}
                    style={{ left: at(step.start), width: `${width.toFixed(3)}%` }}
                  />
                </span>
              </button>
            </li>
          );
        })}
      </ol>
    </div>
  );
}

function Graph({
  members,
  edges,
  states,
  activeEdge,
  selected,
  onSelect,
}: {
  members: TopologyMember[];
  edges: Edge[];
  states: Map<string, { state: MemberState; time?: number }>;
  activeEdge?: string;
  selected?: string;
  onSelect: (name: string) => void;
}) {
  const layout = useMemo(() => place(members, edges), [members, edges]);
  if (members.length === 0) return <p className="dv-empty">Select an agent to see its graph.</p>;
  return (
    <div className="dv-graph" style={{ height: `${layout.height}px` }} role="group" aria-label="Agents and how they hand work to each other">
      <svg className="dv-edges" viewBox={`0 0 100 ${layout.height}`} preserveAspectRatio="none" aria-hidden="true">
        {edges.map((edge) => {
          const from = layout.at.get(edge.from);
          const to = layout.at.get(edge.to);
          if (!from || !to) return null;
          const live = activeEdge === `${edge.from}->${edge.to}`;
          const mid = (from.y + to.y) / 2;
          return (
            <path
              key={`${edge.from}-${edge.to}-${edge.kind}`}
              className={`dv-edge dv-edge-${edge.kind}${live ? " dv-edge-live" : ""}`}
              d={`M${from.x} ${from.y + 26} C${from.x} ${mid},${to.x} ${mid},${to.x} ${to.y - 26}`}
              vectorEffect="non-scaling-stroke"
            />
          );
        })}
      </svg>
      {edges.map((edge) => {
        const from = layout.at.get(edge.from);
        const to = layout.at.get(edge.to);
        if (!from || !to || edge.kind === "contains") return null;
        return (
          <span
            key={`label-${edge.from}-${edge.to}-${edge.kind}`}
            className={`dv-edge-label dv-edge-label-${edge.kind}`}
            style={{ left: `${(from.x + to.x) / 2}%`, top: `${(from.y + to.y) / 2}px` }}
          >
            {edge.kind}
          </span>
        );
      })}
      {members.map((member) => {
        const point = layout.at.get(member.name);
        if (!point) return null;
        const state = states.get(member.name) ?? { state: "idle" as const };
        return (
          <button
            key={member.name}
            type="button"
            className={`dv-node dv-node-${state.state}`}
            aria-pressed={selected === member.name}
            style={{ left: `${point.x}%`, top: `${point.y}px` }}
            onClick={() => onSelect(member.name)}
          >
            <span className="dv-node-mono" aria-hidden="true">
              {visible(member.name).slice(0, 1).toUpperCase()}
            </span>
            <span className="dv-node-text">
              <span className="dv-node-name">{visible(member.name)}</span>
              <span className="dv-node-state">
                {member.coordinator ? "coordinator · " : ""}
                {STATE_LABEL[state.state]}
                {state.state === "done" && state.time !== undefined ? ` · ${duration(state.time)}` : ""}
              </span>
            </span>
          </button>
        );
      })}
    </div>
  );
}

/** Rows by distance from the coordinator along the edges; three to a row when nothing flows. */
function place(members: TopologyMember[], edges: Edge[]) {
  const at = new Map<string, { x: number; y: number }>();
  const coordinator = members.find((member) => member.coordinator) ?? members[0];
  if (!coordinator) return { at, height: 0 };
  const levels = new Map<string, number>([[coordinator.name, 0]]);
  for (let pass = 0; pass < members.length; pass += 1) {
    for (const edge of edges) {
      const level = levels.get(edge.from);
      if (level !== undefined && !levels.has(edge.to)) levels.set(edge.to, level + 1);
    }
  }
  let next = Math.max(0, ...levels.values()) + 1;
  for (const member of members) if (!levels.has(member.name)) levels.set(member.name, next++);
  const rows: TopologyMember[][] = [];
  for (const member of members) {
    const level = levels.get(member.name) ?? 0;
    (rows[level] ??= []).push(member);
  }
  // A wide row wraps at three, so nodes keep their width on a narrow pane.
  const wrapped = rows.filter(Boolean).flatMap((row) => Array.from({ length: Math.ceil(row.length / 3) }, (_, index) => row.slice(index * 3, index * 3 + 3)));
  wrapped.forEach((row, level) => {
    row.forEach((member, index) => at.set(member.name, { x: ((index + 1) * 100) / (row.length + 1), y: 48 + level * 104 }));
  });
  return { at, height: 96 + (wrapped.length - 1) * 104 };
}

function StepInspector({ step, run }: { step: Step; run: Run }) {
  const running = step.end === undefined;
  const length = (step.end ?? run.total) - step.start;
  const facts: Array<[string, string, boolean?]> = [
    ["Duration", running ? `${duration(length)} so far` : duration(length)],
    ["Starts at", duration(step.start)],
    ["Status", step.kind === "held" && running ? "Held for you" : running ? "Running" : "Done", step.kind === "held" && running],
    ["Agent", step.agent ?? "—"],
  ];
  if (step.tokens) facts.push(["Tokens in", tokensText(step.tokens.input)], ["Tokens out", tokensText(step.tokens.output)]);
  return (
    <>
      <div className="dv-inspect-head">
        <span className={`dv-kind-chip dv-kind-${step.kind}`}>{KIND_LABEL[step.kind]}</span>
        <h2 id="dv-inspect-title" className="dv-inspect-title">
          {step.label}
        </h2>
      </div>
      <dl className="dv-facts">
        {facts.map(([key, value, accent]) => (
          <div key={key}>
            <dt>{key}</dt>
            <dd className={accent ? "dv-accent" : undefined}>{value}</dd>
          </div>
        ))}
      </dl>
      {step.detail && (
        <div className="dv-block">
          <h3>{step.detail.label}</h3>
          <pre className="dv-pre">{step.detail.text}</pre>
        </div>
      )}
      {step.attrs.length > 0 && (
        <div className="dv-block">
          <h3>Attributes</h3>
          <dl className="dv-attrs">
            {step.attrs.map(([key, value]) => (
              <div key={key}>
                <dt>{key}</dt>
                <dd>{value}</dd>
              </div>
            ))}
          </dl>
        </div>
      )}
    </>
  );
}

function AgentInspector({ member, name, state }: { member?: TopologyMember; name: string; state?: { state: MemberState; time?: number } }) {
  const capabilities = member ? Object.entries(member.capabilities ?? {}).filter(([, on]) => on).map(([key]) => key) : [];
  return (
    <>
      <div className="dv-inspect-head">
        <span className="dv-kind-chip dv-kind-agent">{member?.coordinator ? "Coordinator" : "Agent"}</span>
        <h2 id="dv-inspect-title" className="dv-inspect-title">
          {visible(name)}
        </h2>
        {member?.description && <p className="dv-inspect-sub">{visible(member.description)}</p>}
      </div>
      <dl className="dv-facts">
        <div>
          <dt>In this run</dt>
          <dd>{STATE_LABEL[state?.state ?? "idle"]}</dd>
        </div>
        <div>
          <dt>Time</dt>
          <dd>{state?.time !== undefined ? duration(state.time) : "—"}</dd>
        </div>
      </dl>
      {capabilities.length > 0 && (
        <div className="dv-block">
          <h3>Capabilities</h3>
          <ul className="dv-caps">
            {capabilities.map((capability) => (
              <li key={capability} className="mono">
                {capability}
              </li>
            ))}
          </ul>
        </div>
      )}
    </>
  );
}

/** Polls `refresh` every `ms` while `on` and the page is shown. */
export function usePoll(on: boolean, ms: number, refresh: () => void) {
  const latest = useRef(refresh);
  latest.current = refresh;
  useEffect(() => {
    if (!on) return;
    const timer = window.setInterval(() => {
      if (document.visibilityState === "visible") latest.current();
    }, ms);
    return () => window.clearInterval(timer);
  }, [on, ms]);
}

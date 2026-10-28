import { useCallback, useEffect, useState } from "react";
import type { Command, CommandOutcome, DecisionCard as Card, Origin } from "../generated/api";
import type { ForkRequest, GraphView, RunStatus, RunStep, StepDetail, WorkflowRun } from "../generated/workflow";
import { ApiError, problemText } from "../client";
import { DecisionCard } from "../components/DecisionCard";
import { Pill } from "../components/Pill";
import type { PillKind } from "../components/Pill";
import { visible } from "../safe";
import type { WorkflowClient } from "./client";
import { ForkDialog } from "./ForkDialog";
import { workflowLabels } from "./labels";
import { GraphLegend, RunGraph } from "./RunGraph";
import { StepDetailView } from "./StepDetail";
import "./workflow.css";

/** What the Runs tab needs of the home: a decision's card from the inbox, and commands. */
export interface RunsHome {
  decision(id: string): Promise<Card>;
  command(command: Command, origin: Origin): Promise<CommandOutcome>;
}

const PILLS: Record<RunStatus, PillKind> = {
  noRun: "neutral",
  notStarted: "neutral",
  working: "decided",
  waiting: "waiting",
  paused: "question",
  stopped: "neutral",
  done: "done",
  failed: "failed",
};

/**
 * The Runs tab of the developer panel (spec 3.3 §12 developer track): a graph agent's latest
 * run in the session the home shows — the run graph, the steps, a step's detail and a fork
 * from it. A decision the run waits on is the inbox's card, drawn by `DecisionCard` and
 * answered with the engine's `decide` command: nothing here draws an answer from the run's
 * state (S-I1).
 */
export function Runs({
  client,
  home,
  graphs,
  agent,
  session,
  onWaiting,
}: {
  client: WorkflowClient;
  home: RunsHome;
  graphs: GraphView[];
  /** The agent the panel shows. */
  agent: string;
  /** The session whose run to show. */
  session?: string;
  /** How many decisions the run waits on, for the tab's badge. */
  onWaiting?: (count: number) => void;
}) {
  const graph = graphs.find((known) => known.name === agent) ?? (graphs.length === 1 && !agent ? graphs[0] : undefined);
  const [viewing, setViewing] = useState(session);
  const [run, setRun] = useState<WorkflowRun>();
  const [failure, setFailure] = useState<{ text: string; request: string }>();
  const [loads, setLoads] = useState(0);
  const [selected, setSelected] = useState<string>();
  const [detail, setDetail] = useState<StepDetail>();
  const [forking, setForking] = useState<StepDetail>();
  const [notice, setNotice] = useState("");
  const [card, setCard] = useState<Card>();

  useEffect(() => setViewing(session), [session]);
  const reload = useCallback(() => setLoads((count) => count + 1), []);

  // The run, read again whenever its thread writes a checkpoint.
  useEffect(() => {
    if (!viewing || !graph) return;
    let live = true;
    setFailure(undefined);
    client
      .run(viewing, graph.name)
      .then((next) => live && setRun(next))
      .catch((error: unknown) => {
        if (!live) return;
        const status = error instanceof ApiError ? ` → ${error.status}` : "";
        setFailure({
          text: problemText(error),
          request: `GET /api/surface/workflow/sessions/${viewing}/run${status}`,
        });
      });
    return () => {
      live = false;
    };
  }, [client, viewing, graph, loads]);

  useEffect(() => {
    if (!viewing || !graph) return;
    const abort = new AbortController();
    void (async () => {
      try {
        for await (const _ of client.changes(viewing, abort.signal)) reload();
      } catch {
        // The feed ends with the page or the server; the next read starts it again.
      }
    })();
    return () => abort.abort();
  }, [client, viewing, graph, reload]);

  // The step shown, read from its checkpoint.
  useEffect(() => {
    if (!viewing || !selected) {
      setDetail(undefined);
      return;
    }
    let live = true;
    client
      .step(viewing, selected)
      .then((next) => live && setDetail(next))
      .catch((error: unknown) => live && setNotice(problemText(error)));
    return () => {
      live = false;
    };
  }, [client, viewing, selected, loads]);

  // The open decision, from the inbox.
  const waiting = run?.nodes.find((node) => node.decisionId)?.decisionId ?? undefined;
  useEffect(() => {
    onWaiting?.(run?.nodes.filter((node) => node.decisionId).length ?? 0);
    if (!waiting) {
      setCard(undefined);
      return;
    }
    let live = true;
    home
      .decision(waiting)
      .then((next) => live && setCard(next))
      .catch(() => live && setCard(undefined));
    return () => {
      live = false;
    };
  }, [home, waiting, run, onWaiting]);

  if (!session) return <p className="muted runs-empty">{workflowLabels.openThread}</p>;
  if (!graph) return <p className="muted runs-empty">{workflowLabels.notAGraph(agent)}</p>;
  if (failure) {
    return (
      <section className="runs-failure" role="alert">
        <h2>{workflowLabels.loadError}</h2>
        <pre className="mono">{visible(failure.request)}</pre>
        <p className="muted">{visible(failure.text)}</p>
        <button type="button" className="button button-secondary" onClick={reload}>
          {workflowLabels.retry}
        </button>
      </section>
    );
  }
  if (!run) {
    return (
      <div className="runs-loading" aria-busy="true">
        <span />
        <span />
        <span />
      </div>
    );
  }

  async function fork(request: ForkRequest) {
    const outcome = await client.fork(run?.sessionId ?? "", request);
    setForking(undefined);
    setSelected(undefined);
    setNotice(outcome.text);
    setViewing(outcome.sessionId);
    return outcome;
  }

  async function openFork(step: RunStep) {
    if (!step.checkpointId || !viewing) return;
    try {
      setForking(await client.step(viewing, step.checkpointId));
    } catch (error) {
      setNotice(problemText(error));
    }
  }

  const send = (command: Command, origin: Origin) => home.command(command, origin);
  return (
    <div className="runs">
      {run.status === "stopped" && <p className="runs-stopped">{workflowLabels.stopped}</p>}
      {notice && (
        <p className="notice notice-done" role="status">
          {visible(notice)}
        </p>
      )}
      {run.lineage && (
        <p className="lineage-chip">
          {visible(run.lineage.text)}
          {run.lineage.parentSession && (
            <>
              {" "}
              <button
                type="button"
                className="link"
                onClick={() => {
                  setNotice("");
                  setSelected(undefined);
                  setViewing(run.lineage?.parentSession ?? viewing);
                }}
              >
                {workflowLabels.openTheRun(run.lineage.text.match(/the (\d\d:\d\d) run/)?.[1] ?? "")}
              </button>
            </>
          )}
        </p>
      )}
      <header className="runs-head">
        <span className="mono runs-graph">{visible(run.graph)}</span>
        <Pill kind={PILLS[run.status]} dot>
          {run.statusText}
        </Pill>
        <span className="runs-summary">{visible(run.summary)}</span>
        <span className="mono muted runs-thread">{visible(run.threadId)}</span>
      </header>
      <div className="runs-body">
        <div className="runs-main">
          <section className="run-graph-card" aria-label={workflowLabels.graph}>
            <header>
              <h2 className="card-label">{workflowLabels.graph}</h2>
              <GraphLegend />
            </header>
            <RunGraph graph={graph} nodes={run.nodes} taken={run.taken} />
            {run.status === "noRun" && <p className="muted">{visible(run.summary)}</p>}
            {run.question && (
              <p className="run-question">
                <span>
                  <span className="mono">{visible(run.nodes.find((node) => node.state === "paused")?.node ?? "")}</span>{" "}
                  {workflowLabels.asked} {visible(run.question)}
                </span>
                <a className="link" href="/ui/">
                  {workflowLabels.answerInThread}
                </a>
              </p>
            )}
            {run.status === "failed" && run.error && <pre className="step-error">{visible(run.error)}</pre>}
          </section>
          {card && card.status === "waiting" && (
            <DecisionCard key={card.id} card={card} send={send} onSettled={reload} />
          )}
          {run.status === "notStarted" && (
            <button
              type="button"
              className="button button-primary"
              onClick={() =>
                void client
                  .start(run.sessionId)
                  .then((outcome) => {
                    setNotice(outcome.text);
                    reload();
                  })
                  .catch((error: unknown) => setNotice(problemText(error)))
              }
            >
              {workflowLabels.startFork}
            </button>
          )}
        </div>
        <aside className="runs-side">
          {detail ? (
            <StepDetailView
              detail={detail}
              onBack={() => setSelected(undefined)}
              onFork={() => setForking(detail)}
            />
          ) : (
            <Steps run={run} selected={selected} onSelect={setSelected} onFork={(step) => void openFork(step)} onOpen={setViewing} />
          )}
        </aside>
      </div>
      {forking && (
        <ForkDialog
          detail={forking}
          channels={graph.channels}
          waitingDecision={waiting}
          onFork={fork}
          onClose={() => setForking(undefined)}
        />
      )}
    </div>
  );
}

/** The run's steps, then the forks made from it. */
function Steps({
  run,
  selected,
  onSelect,
  onFork,
  onOpen,
}: {
  run: WorkflowRun;
  selected?: string;
  onSelect: (checkpoint: string) => void;
  onFork: (step: RunStep) => void;
  onOpen: (session: string) => void;
}) {
  return (
    <section className="run-steps" aria-label={workflowLabels.steps}>
      <h2>{workflowLabels.steps}</h2>
      {run.removedBefore !== undefined && run.removedBefore !== null && (
        <p className="steps-removed">{workflowLabels.removedBefore(run.removedBefore)}</p>
      )}
      <ol>
        {run.steps.map((step) => (
          <li
            key={`${step.step}-${step.state}`}
            className="step-row"
            data-selected={step.checkpointId !== undefined && step.checkpointId === selected}
          >
            <button
              type="button"
              className="step-open"
              disabled={!step.checkpointId}
              onClick={() => step.checkpointId && onSelect(step.checkpointId)}
            >
              <strong>
                {step.label} · <span className="mono">{step.atText}</span>
              </strong>
              <span className="step-line">
                {step.text && step.state === "input" && <span className="muted">{visible(step.text)}</span>}
                {step.nodes.map((node) => (
                  <span key={node.node} className={`node-chip run-node-${node.state}`}>
                    <span className="mono">{visible(node.node)}</span> <span className="node-state">{node.stateText}</span>
                    {node.durationText && <span className="mono duration"> {node.durationText}</span>}
                  </span>
                ))}
                {step.wrote.length > 0 ? (
                  <span>
                    {workflowLabels.wrote} <span className="mono">{step.wrote.map(visible).join(", ")}</span>
                  </span>
                ) : (
                  step.text && step.state !== "input" && <span className="muted">{visible(step.text)}</span>
                )}
              </span>
            </button>
            {step.forkable ? (
              <button type="button" className="fork-here" onClick={() => onFork(step)}>
                {workflowLabels.forkFromHere}
              </button>
            ) : (
              step.state === "input" && (
                <button type="button" className="fork-here" disabled title={workflowLabels.nothingBeforeFirst}>
                  {workflowLabels.forkFromHere}
                </button>
              )
            )}
          </li>
        ))}
      </ol>
      <h2>{workflowLabels.forksOfThisRun}</h2>
      {(run.forks ?? []).length === 0 ? (
        <p className="muted">{workflowLabels.noneYet}</p>
      ) : (
        <ul className="fork-list">
          {(run.forks ?? []).map((fork) => (
            <li key={fork.sessionId}>
              <span className="mono">{fork.atText}</span> <span>{visible(fork.text)}</span>{" "}
              <button type="button" className="link" onClick={() => onOpen(fork.sessionId)}>
                {workflowLabels.open}
              </button>
            </li>
          ))}
        </ul>
      )}
    </section>
  );
}

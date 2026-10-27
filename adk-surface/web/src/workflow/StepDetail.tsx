import { useState } from "react";
import type { NodeDetail, StateChange, StepDetail as Detail } from "../generated/workflow";
import { decisionPath } from "../route";
import { visible, visibleJson } from "../safe";
import { workflowLabels } from "./labels";

/** Values longer than this many lines are collapsed until the person asks for all of them. */
export const COLLAPSED_LINES = 12;

/**
 * One step, read from its checkpoint (spec 3.3 §12): each node with its kind, status, timing
 * and what answered it, then "What changed" by key, Added, Changed or Removed — a word with a
 * colour, never colour alone. Reading it changes nothing.
 */
export function StepDetailView({
  detail,
  onBack,
  onFork,
}: {
  detail: Detail;
  onBack: () => void;
  onFork?: () => void;
}) {
  const everyKeyAdded = detail.changes.every((change) => change.change === "added");
  return (
    <section className="step-detail" aria-label={detail.label}>
      <button type="button" className="link" onClick={onBack}>
        {workflowLabels.backToSteps}
      </button>
      <header className="step-detail-head">
        <h2>
          {detail.label} · <span className="mono">{detail.atText}</span>
        </h2>
        <span className="mono muted">
          {workflowLabels.checkpoint} {visible(detail.checkpointId)}
        </span>
      </header>
      {detail.nodes.map((node) => (
        <NodeBlock key={node.run.node} node={node} />
      ))}
      <h3 className="card-label">{workflowLabels.whatChanged}</h3>
      {detail.changes.length === 0 ? (
        <p className="muted">{workflowLabels.changedNothing}</p>
      ) : (
        <table className="changes">
          <thead>
            <tr>
              <th scope="col">{workflowLabels.key}</th>
              {!everyKeyAdded && <th scope="col">{workflowLabels.before}</th>}
              <th scope="col">{workflowLabels.after}</th>
            </tr>
          </thead>
          <tbody>
            {detail.changes.map((change) => (
              <ChangeRow key={change.key} change={change} withBefore={!everyKeyAdded} />
            ))}
          </tbody>
        </table>
      )}
      {onFork && detail.fork.allowed && (
        <button type="button" className="button button-secondary step-fork" onClick={onFork}>
          {workflowLabels.forkFromHere}
        </button>
      )}
    </section>
  );
}

function NodeBlock({ node }: { node: NodeDetail }) {
  const run = node.run;
  const timing = [
    run.startedText && `${workflowLabels.started} ${run.startedText}`,
    run.durationText,
    run.attempt && workflowLabels.attempt(run.attempt),
  ].filter(Boolean);
  return (
    <article className={`step-node run-node-${run.state}`}>
      <p className="step-node-head">
        <strong className="mono">{visible(run.node)}</strong> <span className="muted">{node.kindText}</span>{" "}
        <span className="node-state">{run.stateText}</span>
      </p>
      {timing.length > 0 && <p className="muted">{timing.join(" · ")}</p>}
      {node.modelStep && (
        <a className="link" href="/ui/">
          {workflowLabels.modelActivity}
        </a>
      )}
      {node.text && <p>{visible(node.text)}</p>}
      {node.effect && node.effect.writes.length > 0 && (
        <p>
          {workflowLabels.wroteAsOriginal} <span className="mono">{node.effect.writes.map(visible).join(", ")}</span>
        </p>
      )}
      {node.decision && (
        <p>
          {visible(node.decision.text)} ·{" "}
          <a className="link" href={decisionPath(node.decision.id)}>
            {workflowLabels.openInDecisions}
          </a>
        </p>
      )}
      {run.error && <pre className="step-error">{visible(run.error)}</pre>}
    </article>
  );
}

function ChangeRow({ change, withBefore }: { change: StateChange; withBefore: boolean }) {
  return (
    <tr>
      <th scope="row">
        <span className="mono">{visible(change.key)}</span>
        <span className={`change-word change-${change.change}`}>{change.changeText}</span>
      </th>
      {withBefore && (
        <td>
          <JsonValue value={change.before} />
        </td>
      )}
      <td>
        <JsonValue value={change.after} />
        {change.note && <p className="muted change-note">{change.note}</p>}
      </td>
    </tr>
  );
}

/** A value as pretty JSON, collapsed past `COLLAPSED_LINES` lines; absent reads "—". */
export function JsonValue({ value }: { value: unknown }) {
  const [all, setAll] = useState(false);
  if (value === undefined) return <span className="muted">—</span>;
  const text = visibleJson(JSON.stringify(value, null, 2) ?? "null");
  const lines = text.split("\n");
  const long = lines.length > COLLAPSED_LINES;
  return (
    <div className="json-value">
      <pre className="mono">{long && !all ? lines.slice(0, COLLAPSED_LINES).join("\n") : text}</pre>
      {long && (
        <button type="button" className="link" onClick={() => setAll((shown) => !shown)}>
          {all ? workflowLabels.showLess : workflowLabels.showAll(lines.length)}
        </button>
      )}
    </div>
  );
}

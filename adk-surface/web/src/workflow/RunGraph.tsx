import type { EdgeView, GraphView, NodeRun } from "../generated/workflow";
import { visible } from "../safe";
import { workflowLabels } from "./labels";
import { END, END_HEIGHT, END_WIDTH, NODE_HEIGHT, NODE_WIDTH, isTaken, layout } from "./layout";

/**
 * The run graph (spec 3.3 §12): each node with its status dot and word, its kind, its duration
 * and its badges ("Asks first", "Spends money", "Can't be undone"); the edges the run took
 * solid, the others muted, conditional ones dashed. A node waiting for a person sits on the
 * decision background. Status is a word and a colour, never colour alone (§9).
 */
export function RunGraph({ graph, nodes, taken }: { graph: GraphView; nodes: NodeRun[]; taken: EdgeView[] }) {
  const placed = layout(graph);
  const runOf = new Map(nodes.map((node) => [node.node, node]));
  const viewOf = new Map(graph.nodes.map((node) => [node.name, node]));
  const finished = graph.edges.some((edge) => edge.to === END && isTaken(edge, taken));
  return (
    <svg
      className="run-graph"
      viewBox={`0 0 ${placed.width} ${placed.height}`}
      role="img"
      aria-label={`${graph.name}: ${nodes.map((node) => `${node.node} ${node.stateText}`).join(", ")}`}
    >
      <defs>
        <marker id="run-arrow" viewBox="0 0 10 10" refX="8" refY="5" markerWidth="5" markerHeight="5" orient="auto-start-reverse">
          <path d="M 0 0 L 10 5 L 0 10 z" />
        </marker>
        <marker id="run-arrow-taken" viewBox="0 0 10 10" refX="8" refY="5" markerWidth="5" markerHeight="5" orient="auto-start-reverse">
          <path className="taken" d="M 0 0 L 10 5 L 0 10 z" />
        </marker>
      </defs>
      {placed.edges.map(({ edge, path }) => {
        const on = isTaken(edge, taken);
        return (
          <path
            key={`${edge.from}->${edge.to}`}
            data-edge={`${edge.from}->${edge.to}`}
            className={`run-edge${on ? " run-edge-taken" : ""}${edge.conditional ? " run-edge-conditional" : ""}`}
            d={path}
            markerEnd={on ? "url(#run-arrow-taken)" : "url(#run-arrow)"}
          />
        );
      })}
      {placed.nodes.map((point) => {
        const run = runOf.get(point.name);
        const view = viewOf.get(point.name);
        const state = run?.state ?? "idle";
        // A node a fork did not run again shows when the original did it in place of a time.
        const chip = run?.durationText ?? run?.inheritedText ?? undefined;
        const under = run?.stateText ?? "";
        const chipWidth = (chip?.length ?? 0) * 6.4 + 12;
        return (
          <g key={point.name} className={`run-node run-node-${state}`} transform={`translate(${point.x} ${point.y})`}>
            <rect width={NODE_WIDTH} height={NODE_HEIGHT} rx="9" />
            <circle className="run-dot" cx="16" cy="19" r="5.5" />
            <text className="run-node-name" x="29" y="23">
              {visible(point.name)}
            </text>
            <text className="run-node-kind" x="13" y="44">
              {view?.kindText ?? ""}
            </text>
            <text className="run-state" x="2" y={NODE_HEIGHT + 20}>
              {visible(under)}
            </text>
            {chip && (
              <g transform={`translate(${8 + under.length * 6.6} ${NODE_HEIGHT + 8})`}>
                <rect className="run-chip" width={chipWidth} height="17" rx="4" />
                <text className="run-chip-text" x={chipWidth / 2} y="12.5" textAnchor="middle">
                  {chip}
                </text>
              </g>
            )}
            {(view?.badges ?? []).map((badge, index) => (
              <g key={badge} transform={`translate(2 ${NODE_HEIGHT + 30 + index * 19})`}>
                <rect className="run-badge" width={badge.length * 6 + 12} height="16" rx="3" />
                <text className="run-badge-text" x="6" y="11.5">
                  {badge}
                </text>
              </g>
            ))}
          </g>
        );
      })}
      {placed.end && (
        <g className={`run-end${finished ? " run-end-reached" : ""}`} transform={`translate(${placed.end.x} ${placed.end.y})`}>
          <rect width={END_WIDTH} height={END_HEIGHT} rx={END_HEIGHT / 2} />
          <text x={END_WIDTH / 2} y="15" textAnchor="middle">
            END
          </text>
        </g>
      )}
    </svg>
  );
}

/** The legend over the graph: taken, not taken, conditional. */
export function GraphLegend() {
  return (
    <div className="run-legend" aria-hidden="true">
      <span>
        <i className="legend-taken" /> {workflowLabels.taken}
      </span>
      <span>
        <i className="legend-untaken" /> {workflowLabels.notTaken}
      </span>
      <span>
        <i className="legend-conditional" /> {workflowLabels.conditional}
      </span>
    </div>
  );
}

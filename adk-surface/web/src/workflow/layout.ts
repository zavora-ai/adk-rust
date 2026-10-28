/**
 * The run graph's layered layout (spec 3.3 §12; owner question 5): deterministic and free of
 * dependencies, in the Topology tab's style. Each node's layer is its distance from the entry
 * nodes; nodes of one layer stack by name; END sits after the last layer. An edge between
 * neighbouring layers is a straight line, one that skips layers arcs above, and one that goes
 * back arcs below.
 */
import type { EdgeView, GraphView } from "../generated/workflow";

/** The END node's name in a graph's edges. */
export const END = "__end__";

export const NODE_WIDTH = 146;
export const NODE_HEIGHT = 58;
const GAP_X = 70;
const GAP_Y = 108;
const PAD_X = 16;
const PAD_TOP = 44;
/** Room under each row for the status line and the badges. */
const BELOW = 88;
export const END_WIDTH = 46;
export const END_HEIGHT = 22;

export interface Placed {
  name: string;
  x: number;
  y: number;
}

export interface Routed {
  edge: EdgeView;
  path: string;
}

export interface Layout {
  width: number;
  height: number;
  nodes: Placed[];
  end?: Placed;
  edges: Routed[];
}

/** Lays out `graph`. */
export function layout(graph: GraphView): Layout {
  const names = graph.nodes.map((node) => node.name);
  const level = new Map<string, number>();
  const queue: string[] = [];
  for (const entry of [...graph.entry].sort()) {
    if (names.includes(entry) && !level.has(entry)) {
      level.set(entry, 0);
      queue.push(entry);
    }
  }
  while (queue.length) {
    const from = queue.shift() as string;
    const next = (level.get(from) ?? 0) + 1;
    for (const edge of graph.edges) {
      if (edge.from === from && edge.to !== END && names.includes(edge.to) && !level.has(edge.to)) {
        level.set(edge.to, next);
        queue.push(edge.to);
      }
    }
  }
  let deepest = Math.max(-1, ...level.values());
  for (const name of [...names].sort()) {
    if (!level.has(name)) level.set(name, ++deepest);
  }
  const layers = Math.max(0, ...level.values()) + 1;
  const rows = new Map<number, string[]>();
  for (const name of [...names].sort()) {
    const at = level.get(name) ?? 0;
    rows.set(at, [...(rows.get(at) ?? []), name]);
  }
  const tallest = Math.max(1, ...[...rows.values()].map((row) => row.length));
  const nodes: Placed[] = [];
  for (const [at, row] of rows) {
    row.forEach((name, index) => {
      nodes.push({ name, x: PAD_X + at * (NODE_WIDTH + GAP_X), y: PAD_TOP + index * (NODE_HEIGHT + GAP_Y) });
    });
  }
  const reachesEnd = graph.edges.some((edge) => edge.to === END);
  const end: Placed | undefined = reachesEnd
    ? { name: END, x: PAD_X + layers * (NODE_WIDTH + GAP_X), y: PAD_TOP + (NODE_HEIGHT - END_HEIGHT) / 2 }
    : undefined;
  const at = new Map(nodes.map((node) => [node.name, node]));
  const layerOf = (name: string) => (name === END ? layers : (level.get(name) ?? 0));
  const edges: Routed[] = [];
  for (const edge of graph.edges) {
    const from = at.get(edge.from);
    const to = edge.to === END ? end : at.get(edge.to);
    if (!from || !to) continue;
    const toWidth = edge.to === END ? END_WIDTH : NODE_WIDTH;
    const toHeight = edge.to === END ? END_HEIGHT : NODE_HEIGHT;
    const span = layerOf(edge.to) - layerOf(edge.from);
    let path: string;
    if (span === 1 && Math.abs(from.y - (edge.to === END ? PAD_TOP : to.y)) < 1) {
      const y = from.y + NODE_HEIGHT / 2;
      path = `M${from.x + NODE_WIDTH} ${y} L${to.x - 4} ${y}`;
    } else if (span >= 1) {
      // Skips layers, or changes rows: an arc over the nodes between.
      const startX = from.x + NODE_WIDTH / 2;
      const endX = to.x + toWidth / 2;
      const top = Math.min(from.y, to.y) - 30;
      path = `M${startX} ${from.y} C${startX} ${top},${endX} ${top},${endX} ${to.y - 4}`;
    } else {
      // Back to an earlier layer: an arc under the nodes.
      const startX = from.x + NODE_WIDTH / 2;
      const endX = to.x + toWidth / 2;
      const bottom = Math.max(from.y + NODE_HEIGHT, to.y + toHeight) + BELOW - 20;
      path = `M${startX} ${from.y + NODE_HEIGHT} C${startX} ${bottom},${endX} ${bottom},${endX} ${to.y + toHeight + 4}`;
    }
    edges.push({ edge, path });
  }
  const width = PAD_X * 2 + layers * (NODE_WIDTH + GAP_X) + (end ? END_WIDTH : -GAP_X);
  const height = PAD_TOP + tallest * NODE_HEIGHT + (tallest - 1) * GAP_Y + BELOW;
  return { width, height, nodes, end, edges };
}

/** Whether the run took `edge`. */
export function isTaken(edge: EdgeView, taken: EdgeView[]): boolean {
  return taken.some((known) => known.from === edge.from && known.to === edge.to);
}

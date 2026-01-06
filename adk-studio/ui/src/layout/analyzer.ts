import type { Node, Edge } from '@xyflow/react';
import type { LayoutMode } from '../types/layout';

export interface GraphAnalysis {
  nodeCount: number;
  edgeCount: number;
  maxDepth: number;
  hasRouter: boolean;
  dominantPattern: LayoutMode;
  entryPoints: string[];
  exitPoints: string[];
}

export function analyzeGraph(nodes: Node[], edges: Edge[]): GraphAnalysis {
  const nodeIds = new Set(nodes.map(n => n.id));
  const adjacency = new Map<string, string[]>();
  const inDegree = new Map<string, number>();

  nodes.forEach(n => { adjacency.set(n.id, []); inDegree.set(n.id, 0); });
  edges.forEach(e => {
    if (nodeIds.has(e.source) && nodeIds.has(e.target)) {
      adjacency.get(e.source)!.push(e.target);
      inDegree.set(e.target, (inDegree.get(e.target) || 0) + 1);
    }
  });

  const entryPoints = nodes.filter(n => inDegree.get(n.id) === 0).map(n => n.id);
  const exitPoints = nodes.filter(n => adjacency.get(n.id)!.length === 0).map(n => n.id);
  const hasRouter = nodes.some(n => n.type === 'router');
  const isLinear = edges.length === nodes.length - 1 && entryPoints.length === 1;

  let dominantPattern: LayoutMode = 'freeform';
  if (isLinear && !hasRouter) dominantPattern = 'pipeline';
  else if (hasRouter) dominantPattern = 'tree';

  const maxDepth = calculateMaxDepth(adjacency, entryPoints);

  return { nodeCount: nodes.length, edgeCount: edges.length, maxDepth, hasRouter, dominantPattern, entryPoints, exitPoints };
}

function calculateMaxDepth(adjacency: Map<string, string[]>, entryPoints: string[]): number {
  const depths = new Map<string, number>();
  function dfs(node: string, depth: number) {
    depths.set(node, Math.max(depths.get(node) || 0, depth));
    for (const neighbor of adjacency.get(node) || []) dfs(neighbor, depth + 1);
  }
  entryPoints.forEach(entry => dfs(entry, 0));
  return Math.max(...depths.values(), 0);
}

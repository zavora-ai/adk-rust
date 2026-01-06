import { useCallback } from 'react';
import { useReactFlow } from '@xyflow/react';
import dagre from 'dagre';
import type { LayoutDirection } from '../types/layout';
import { useStore } from '../store';

export function useLayout() {
  const { getNodes, getEdges, setNodes, fitView } = useReactFlow();
  const layoutDirection = useStore(s => s.layoutDirection);
  const setLayoutDirection = useStore(s => s.setLayoutDirection);
  const selectedNodeId = useStore(s => s.selectedNodeId);

  // Padding accounts for side panel (~320px) when node is selected
  const getPadding = useCallback(() => {
    return selectedNodeId ? { top: 0.1, left: 0.1, bottom: 0.1, right: 0.35 } : 0.1;
  }, [selectedNodeId]);

  const doLayout = useCallback((direction: LayoutDirection) => {
    const nodes = getNodes();
    const edges = getEdges();
    if (nodes.length === 0) return;

    const g = new dagre.graphlib.Graph();
    g.setGraph({ rankdir: direction, nodesep: 40, ranksep: 100 });
    g.setDefaultEdgeLabel(() => ({}));

    nodes.forEach(node => g.setNode(node.id, { width: 180, height: 100 }));
    edges.forEach(edge => g.setEdge(edge.source, edge.target));
    dagre.layout(g);

    setNodes(nodes.map(node => {
      const pos = g.node(node.id);
      return { ...node, position: { x: pos.x - 90, y: pos.y - 50 } };
    }));

    setTimeout(() => fitView({ padding: getPadding(), maxZoom: 0.9 }), 50);
  }, [getNodes, getEdges, setNodes, fitView, getPadding]);

  // Toggle layout direction
  const toggleLayout = useCallback(() => {
    const newDirection: LayoutDirection = layoutDirection === 'LR' ? 'TB' : 'LR';
    setLayoutDirection(newDirection);
    doLayout(newDirection);
  }, [layoutDirection, setLayoutDirection, doLayout]);

  // Apply layout without toggling (uses current direction)
  const applyLayout = useCallback(() => {
    doLayout(layoutDirection);
  }, [doLayout, layoutDirection]);

  const fitToView = useCallback(() => fitView({ padding: getPadding(), duration: 300, maxZoom: 0.9 }), [fitView, getPadding]);

  return { applyLayout, toggleLayout, fitToView, layoutDirection };
}

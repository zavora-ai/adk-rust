import { useCallback, DragEvent } from 'react';
import { useReactFlow, type Node } from '@xyflow/react';
import { useStore } from '../store';
import type { ActionNodeType, ActionNodeConfig } from '../types/actionNodes';
import { createDefaultStandardProperties } from '../types/standardProperties';
import type { AutobuildTriggerType } from './useBuild';
import type { LayoutDirection } from '../types/layout';

/**
 * Ephemeral drop position — consumed by useCanvasNodes when building new nodes.
 * This avoids polluting the store with transient UI state.
 */
let _pendingDropPosition: { x: number; y: number } | null = null;

/** Read and consume the pending drop position (one-shot). */
export function consumePendingDropPosition(): { x: number; y: number } | null {
  const pos = _pendingDropPosition;
  _pendingDropPosition = null;
  return pos;
}

/**
 * Parameters for the useCanvasDragDrop hook.
 */
export interface UseCanvasDragDropParams {
  /** Callback to create an agent with undo support. Returns the new agent ID. */
  createAgentWithUndo: (agentType?: string, skipWiring?: boolean) => string | undefined;
  /** Currently selected agent node ID */
  selectedNodeId: string | null;
  /** Callback to apply layout after node changes */
  applyLayout: () => void;
  /** Callback to invalidate the current build */
  invalidateBuild: (reason?: AutobuildTriggerType) => void;
}

/**
 * Return type for the useCanvasDragDrop hook.
 */
export interface UseCanvasDragDropReturn {
  /** Handler for agent palette drag start */
  onDragStart: (e: DragEvent, type: string) => void;
  /** Handler for action node palette drag start */
  onActionDragStart: (e: DragEvent, type: ActionNodeType) => void;
  /** Handler for drag over the canvas */
  onDragOver: (e: DragEvent) => void;
  /** Handler for dropping items on the canvas */
  onDrop: (e: DragEvent) => void;
  /** Create an action node and wire it into the workflow */
  createActionNode: (type: ActionNodeType) => void;
}

/**
 * Compute the shortest distance from point (px, py) to the line segment (ax,ay)→(bx,by).
 * Also returns `t` — the projection parameter (0 = at source, 1 = at target).
 */
function pointToSegmentDist(
  px: number, py: number,
  ax: number, ay: number,
  bx: number, by: number,
): { dist: number; t: number } {
  const dx = bx - ax;
  const dy = by - ay;
  const lenSq = dx * dx + dy * dy;
  // Degenerate segment (source == target)
  if (lenSq === 0) return { dist: Math.sqrt((px - ax) ** 2 + (py - ay) ** 2), t: 0 };
  // Project point onto line, clamped to [0,1]
  const t = Math.max(0, Math.min(1, ((px - ax) * dx + (py - ay) * dy) / lenSq));
  const projX = ax + t * dx;
  const projY = ay + t * dy;
  return { dist: Math.sqrt((px - projX) ** 2 + (py - projY) ** 2), t };
}

/**
 * Find the closest edge to a drop position using point-to-segment distance.
 * This means dropping *anywhere* between two connected nodes will find that edge,
 * not just near the midpoint. Returns null only if no edge is within threshold.
 */
function findClosestEdge(
  dropX: number,
  dropY: number,
  edges: Array<{ from: string; to: string }>,
  nodePositions: Map<string, { x: number; y: number }>,
  threshold: number = 200,
): { from: string; to: string } | null {
  let closest: { from: string; to: string } | null = null;
  let closestDist = threshold;

  for (const edge of edges) {
    const sourcePos = nodePositions.get(edge.from);
    const targetPos = nodePositions.get(edge.to);
    if (!sourcePos || !targetPos) continue;

    const { dist, t } = pointToSegmentDist(
      dropX, dropY,
      sourcePos.x, sourcePos.y,
      targetPos.x, targetPos.y,
    );

    // Require t to be between 0.05 and 0.95 — don't split if dropping
    // right on top of the source or target node itself
    if (dist < closestDist && t > 0.05 && t < 0.95) {
      closestDist = dist;
      closest = edge;
    }
  }

  return closest;
}

/** Minimum spacing (px) between nodes along the main layout axis after edge-split insertion. */
const EDGE_SPLIT_SPACING = 220;

/**
 * After inserting a node via edge-split, ensure proper spacing between the
 * source → inserted → downstream chain. Repositions the inserted node at the
 * midpoint between its source and the first downstream node, then shifts all
 * downstream nodes if there still isn't enough room.
 *
 * @param insertedNodeId  The node that was just inserted
 * @param dropPosition    Where the inserted node was placed (flow coords)
 * @param direction       Current layout direction ('LR' or 'TB')
 * @param getNodes        ReactFlow getNodes
 * @param setNodes        ReactFlow setNodes
 * @param projectEdges    Current project workflow edges (after split)
 */
function autoSpaceAfterSplit(
  insertedNodeId: string,
  dropPosition: { x: number; y: number },
  direction: LayoutDirection,
  getNodes: () => Node[],
  setNodes: (updater: (nodes: Node[]) => Node[]) => void,
  projectEdges: Array<{ from: string; to: string }>,
) {
  const nodes = getNodes();
  const isHorizontal = direction === 'LR' || direction === 'RL';

  // Build position map
  const posMap = new Map<string, { x: number; y: number; w: number; h: number }>();
  for (const n of nodes) {
    const w = (n.measured?.width ?? n.width ?? 180) as number;
    const h = (n.measured?.height ?? n.height ?? 80) as number;
    posMap.set(n.id, { x: n.position.x, y: n.position.y, w, h });
  }

  // Build forward adjacency
  const forward: Record<string, string[]> = {};
  for (const e of projectEdges) {
    if (!forward[e.from]) forward[e.from] = [];
    forward[e.from].push(e.to);
  }

  // Find the source node (who connects TO the inserted node)
  const sourceEdge = projectEdges.find(e => e.to === insertedNodeId);
  const sourceId = sourceEdge?.from;
  const sourcePos = sourceId ? posMap.get(sourceId) : null;

  // BFS from insertedNode to collect all downstream node IDs
  const downstream = new Set<string>();
  const visited = new Set<string>([insertedNodeId]);
  const startTargets = forward[insertedNodeId] || [];
  for (const t of startTargets) {
    if (!visited.has(t)) { visited.add(t); downstream.add(t); }
  }
  const bfsQueue = [...downstream];
  while (bfsQueue.length > 0) {
    const current = bfsQueue.shift()!;
    for (const next of (forward[current] || [])) {
      if (!visited.has(next)) {
        visited.add(next);
        downstream.add(next);
        bfsQueue.push(next);
      }
    }
  }

  if (downstream.size === 0) return;

  // Get main-axis positions
  const axis = (p: { x: number; y: number }) => isHorizontal ? p.x : p.y;
  const insertedMainAxis = axis(dropPosition);

  // Find the closest downstream node along the main axis
  let closestDownstreamPos = Infinity;
  for (const id of downstream) {
    const p = posMap.get(id);
    if (!p) continue;
    const pos = axis(p);
    if (pos < closestDownstreamPos) closestDownstreamPos = pos;
  }

  // Calculate how much to shift downstream nodes
  const gapToDownstream = closestDownstreamPos - insertedMainAxis;
  const shiftNeeded = EDGE_SPLIT_SPACING - gapToDownstream;

  // Also check if the inserted node is too close to its source
  let insertedShift = 0;
  if (sourcePos) {
    const sourceMainAxis = axis(sourcePos) + (isHorizontal ? (sourcePos.w || 180) : (sourcePos.h || 80));
    const gapFromSource = insertedMainAxis - sourceMainAxis;
    if (gapFromSource < EDGE_SPLIT_SPACING * 0.4) {
      // Too close to source — nudge the inserted node forward
      insertedShift = EDGE_SPLIT_SPACING * 0.4 - gapFromSource;
    }
  }

  // Total shift for downstream = shift needed + any inserted node nudge
  const totalDownstreamShift = Math.max(0, shiftNeeded) + insertedShift;

  if (totalDownstreamShift <= 0 && insertedShift <= 0) return;

  setNodes((nds) =>
    nds.map((n) => {
      // Nudge the inserted node if it's too close to source
      if (n.id === insertedNodeId && insertedShift > 0) {
        return {
          ...n,
          position: isHorizontal
            ? { x: n.position.x + insertedShift, y: n.position.y }
            : { x: n.position.x, y: n.position.y + insertedShift },
        };
      }
      // Shift all downstream nodes
      if (downstream.has(n.id) && totalDownstreamShift > 0) {
        return {
          ...n,
          position: isHorizontal
            ? { x: n.position.x + totalDownstreamShift, y: n.position.y }
            : { x: n.position.x, y: n.position.y + totalDownstreamShift },
        };
      }
      return n;
    })
  );
}

/**
 * Hook that encapsulates all drag-and-drop handlers for the Canvas.
 *
 * Key behaviors:
 * - Nodes are placed at the drop cursor position (n8n-style)
 * - Dropping near an edge midpoint splits the edge (insert between nodes)
 * - After edge-split, downstream nodes are auto-spaced to prevent overlap
 * - Auto-layout is only applied when no drop position is available (palette click)
 * - Node positions are persisted and respected
 *
 * @see Requirements 2.5
 */
export function useCanvasDragDrop({
  createAgentWithUndo,
  selectedNodeId,
  applyLayout,
  invalidateBuild,
}: UseCanvasDragDropParams): UseCanvasDragDropReturn {
  const { screenToFlowPosition, getNodes, setNodes } = useReactFlow();
  const addActionNode = useStore(s => s.addActionNode);
  const addProjectEdge = useStore(s => s.addEdge);
  const removeProjectEdge = useStore(s => s.removeEdge);
  const selectActionNode = useStore(s => s.selectActionNode);
  const addToolToAgent = useStore(s => s.addToolToAgent);

  // Agent palette drag start handler
  const onDragStart = (e: DragEvent, type: string) => {
    e.dataTransfer.setData('application/reactflow', type);
    e.dataTransfer.effectAllowed = 'move';
  };

  // Action node palette drag start handler
  const onActionDragStart = (e: DragEvent, type: ActionNodeType) => {
    e.dataTransfer.setData('application/actionnode', type);
    e.dataTransfer.effectAllowed = 'move';
  };

  /**
   * Wire a new node into the workflow graph.
   * If dropPosition is provided and close to an edge, splits that edge.
   * Otherwise appends before END.
   */
  const wireNodeIntoWorkflow = useCallback((
    nodeId: string,
    isTrigger: boolean,
    dropPosition?: { x: number; y: number },
  ) => {
    const currentProject = useStore.getState().currentProject;
    if (!currentProject) return;

    if (isTrigger) {
      // Trigger nodes connect TO START
      const existingTrigger = Object.values(currentProject.actionNodes || {}).find(
        (node) => node.type === 'trigger'
      );
      if (existingTrigger && existingTrigger.id !== nodeId) {
        useStore.getState().removeActionNode(nodeId);
        alert('Only one trigger node is allowed per workflow. Remove the existing trigger first.');
        return;
      }
      addProjectEdge(nodeId, 'START');
      return;
    }

    // Build node center positions from ReactFlow nodes for edge proximity detection.
    // We use measured dimensions when available, otherwise sensible defaults.
    const nodes = getNodes();
    const nodePositions = new Map<string, { x: number; y: number }>();
    for (const n of nodes) {
      const w = (n.measured?.width ?? n.width ?? 180) as number;
      const h = (n.measured?.height ?? n.height ?? 80) as number;
      nodePositions.set(n.id, { x: n.position.x + w / 2, y: n.position.y + h / 2 });
    }

    // Always try to find the closest edge to the drop position and split it.
    // This is the core n8n-style behavior: drop between two nodes → insert there.
    // Exclude trigger→START edges — those are entry-point wiring, not splittable.
    if (dropPosition) {
      const splittableEdges = currentProject.workflow.edges.filter(e => e.to !== 'START');
      const edgeToSplit = findClosestEdge(
        dropPosition.x,
        dropPosition.y,
        splittableEdges,
        nodePositions,
      );

      if (edgeToSplit) {
        // Split: remove old edge, insert node in between
        removeProjectEdge(edgeToSplit.from, edgeToSplit.to);
        addProjectEdge(edgeToSplit.from, nodeId);
        addProjectEdge(nodeId, edgeToSplit.to);

        // Auto-space downstream nodes so nothing overlaps after insertion
        setTimeout(() => {
          const updatedEdges = useStore.getState().currentProject?.workflow.edges || [];
          const dir = useStore.getState().layoutDirection;
          autoSpaceAfterSplit(nodeId, dropPosition, dir, getNodes, setNodes, updatedEdges);
        }, 50);
        return;
      }
    }

    // Fallback: no edge was close enough. Append before END.
    const edgeToEnd = currentProject.workflow.edges.find(e => e.to === 'END');
    if (edgeToEnd) {
      removeProjectEdge(edgeToEnd.from, 'END');
      addProjectEdge(edgeToEnd.from, nodeId);
    } else {
      addProjectEdge('START', nodeId);
    }
    addProjectEdge(nodeId, 'END');
  }, [addProjectEdge, removeProjectEdge, getNodes, setNodes]);

  // Create action node handler — used by palette click (no drop position)
  const createActionNode = useCallback((type: ActionNodeType, dropPosition?: { x: number; y: number }) => {
    const currentProject = useStore.getState().currentProject;
    if (!currentProject) return;

    const id = `${type}_${Date.now()}`;
    const name = type.charAt(0).toUpperCase() + type.slice(1);
    const baseProps = createDefaultStandardProperties(id, name, `${type}Result`);

    let nodeConfig: ActionNodeConfig;

    switch (type) {
      case 'trigger':
        nodeConfig = { ...baseProps, type: 'trigger', triggerType: 'manual' };
        break;
      case 'http':
        nodeConfig = {
          ...baseProps, type: 'http', method: 'GET', url: 'https://api.example.com',
          auth: { type: 'none' }, headers: {}, body: { type: 'none' }, response: { type: 'json' },
        };
        break;
      case 'set':
        nodeConfig = { ...baseProps, type: 'set', mode: 'set', variables: [] };
        break;
      case 'transform':
        nodeConfig = { ...baseProps, type: 'transform', transformType: 'jsonpath', expression: '' };
        break;
      case 'switch':
        nodeConfig = { ...baseProps, type: 'switch', evaluationMode: 'first_match', conditions: [] };
        break;
      case 'loop':
        nodeConfig = {
          ...baseProps, type: 'loop', loopType: 'forEach',
          forEach: { sourceArray: '', itemVar: 'item', indexVar: 'index' },
          parallel: { enabled: false }, results: { collect: true },
        };
        break;
      case 'merge':
        nodeConfig = {
          ...baseProps, type: 'merge', mode: 'wait_all', combineStrategy: 'array',
          timeout: { enabled: false, ms: 30000, behavior: 'error' },
        };
        break;
      case 'wait':
        nodeConfig = {
          ...baseProps, type: 'wait', waitType: 'fixed',
          fixed: { duration: 1000, unit: 'ms' },
        };
        break;
      case 'code':
        nodeConfig = {
          ...baseProps, type: 'code', language: 'javascript',
          code: '// Your code here\nreturn input;',
          sandbox: { networkAccess: false, fileSystemAccess: false, memoryLimit: 128, timeLimit: 5000 },
        };
        break;
      case 'database':
        nodeConfig = {
          ...baseProps, type: 'database', dbType: 'postgresql',
          connection: { connectionString: '' },
        };
        break;
      case 'email':
        nodeConfig = {
          ...baseProps, type: 'email', mode: 'send',
          smtp: { host: 'smtp.example.com', port: 587, secure: true, username: '', password: '', fromEmail: '' },
          recipients: { to: '' },
          content: { subject: '', body: '', bodyType: 'text' },
          attachments: [],
        };
        break;
      default:
        return;
    }

    addActionNode(id, nodeConfig);
    wireNodeIntoWorkflow(id, type === 'trigger', dropPosition);
    selectActionNode(id);
    invalidateBuild('onAgentAdd');

    // Only auto-layout if no drop position (palette click, not drag-drop)
    if (!dropPosition) {
      setTimeout(() => applyLayout(), 100);
    }
  }, [addActionNode, wireNodeIntoWorkflow, selectActionNode, applyLayout, invalidateBuild]);

  const onDragOver = useCallback((e: DragEvent) => {
    e.preventDefault();
    e.dataTransfer.dropEffect = e.dataTransfer.types.includes('text/plain') ? 'copy' : 'move';
  }, []);

  const onDrop = useCallback((e: DragEvent) => {
    e.preventDefault();

    const currentProject = useStore.getState().currentProject;

    // Convert screen coordinates to flow coordinates for drop-at-cursor
    const flowPosition = screenToFlowPosition({ x: e.clientX, y: e.clientY });

    // Handle tool drop onto selected agent
    const toolData = e.dataTransfer.getData('text/plain');
    if (toolData.startsWith('tool:') && selectedNodeId && currentProject?.agents[selectedNodeId]) {
      addToolToAgent(selectedNodeId, toolData.slice(5));
      invalidateBuild('onToolAdd');
      return;
    }

    // Handle action node drop — place at cursor position
    const actionType = e.dataTransfer.getData('application/actionnode');
    if (actionType) {
      _pendingDropPosition = flowPosition;
      createActionNode(actionType as ActionNodeType, flowPosition);
      return;
    }

    // Handle agent drop — place at cursor position and wire via edge-splitting
    const type = e.dataTransfer.getData('application/reactflow');
    if (type) {
      _pendingDropPosition = flowPosition;
      const newAgentId = createAgentWithUndo(type, true); // skipWiring=true, we'll wire it ourselves
      if (newAgentId) {
        wireNodeIntoWorkflow(newAgentId, false, flowPosition);
      }
      invalidateBuild('onAgentAdd');
      // Don't auto-layout — node will be placed at drop position
    }
  }, [createAgentWithUndo, createActionNode, selectedNodeId, addToolToAgent, invalidateBuild, screenToFlowPosition]);

  return {
    onDragStart,
    onActionDragStart,
    onDragOver,
    onDrop,
    createActionNode,
  };
}

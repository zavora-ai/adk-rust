import { useEffect, useRef, useMemo } from 'react';
import { Node, Edge, useNodesState, useEdgesState } from '@xyflow/react';
import type { Project, Edge as WorkflowEdge } from '../types/project';
import type { ActionNodeConfig } from '../types/actionNodes';
import { useStore } from '../store';
import { consumePendingDropPosition } from './useCanvasDragDrop';

interface ExecutionState {
  activeAgent: string | null;
  iteration: number;
  /** 
   * Flow phase for edge animations.
   * - 'idle': No activity
   * - 'trigger_input': User submitting input to trigger (animates trigger→START)
   * - 'input': Data flowing from START to agents
   * - 'output': Agent generating response
   * - 'interrupted': Waiting for HITL response
   * @see trigger-input-flow Requirements 2.2, 2.3, 3.1, 3.2
   */
  flowPhase: 'idle' | 'trigger_input' | 'input' | 'output' | 'interrupted';
  thoughts?: Record<string, string>;
  /** v2.0: State keys from SSE events for data flow overlays (nodeId -> keys) */
  stateKeys?: Map<string, string[]>;
  /** v2.0: Whether to show data flow overlay */
  showDataFlowOverlay?: boolean;
  /** v2.0: Currently highlighted state key (for hover highlighting) */
  highlightedKey?: string | null;
  /** v2.0: Callback when a state key is hovered */
  onKeyHover?: (key: string | null) => void;
  /** v2.0: Execution path for highlighting (ordered list of node IDs) */
  executionPath?: string[];
  /** v2.0: Whether execution is in progress */
  isExecuting?: boolean;
  /** 
   * HITL: Node ID that triggered the interrupt (for visual indicator).
   * When set, the node with this ID will show the interrupted visual state.
   * @see trigger-input-flow Requirement 3.3: Interrupt visual indicator
   */
  interruptedNodeId?: string | null;
}

/**
 * Maps action node type to ReactFlow node type key.
 * Action nodes use 'action_' prefix to avoid conflicts with agent node types.
 */
function getActionNodeType(actionType: ActionNodeConfig['type']): string {
  return `action_${actionType}`;
}

/**
 * Generate a stable hash for detecting structural changes.
 * This ensures we only rebuild nodes when the actual structure changes.
 */
function getStructureHash(project: Project | null): string {
  if (!project) return '';
  
  const agentKeys = Object.keys(project.agents).sort().join(',');
  const actionNodeKeys = Object.keys(project.actionNodes || {}).sort().join(',');
  const modelsHash = Object.entries(project.agents)
    .map(([id, a]) => `${id}:${a.model || ''}`)
    .sort()
    .join('|');
  const toolsHash = Object.entries(project.agents)
    .map(([id, a]) => `${id}:${(a.tools || []).join('+')}`)
    .sort()
    .join('|');
  const edgesHash = project.workflow.edges
    .map(e => `${e.from}->${e.to}`)
    .sort()
    .join(',');
  
  return `${agentKeys}|${actionNodeKeys}|${modelsHash}|${toolsHash}|${edgesHash}`;
}

export function useCanvasNodes(project: Project | null, execution: ExecutionState) {
  const [nodes, setNodes, onNodesChange] = useNodesState<Node>([]);
  const [edges, setEdges, onEdgesChange] = useEdgesState<Edge>([]);
  const { 
    activeAgent, 
    iteration, 
    flowPhase, 
    thoughts = {}, 
    stateKeys, 
    showDataFlowOverlay, 
    highlightedKey, 
    onKeyHover,
    executionPath = [],
    isExecuting = false,
    interruptedNodeId = null,
  } = execution;
  const layoutDirection = useStore(s => s.layoutDirection);
  const isHorizontal = layoutDirection === 'LR' || layoutDirection === 'RL';
  
  // Track structure hash to detect actual changes
  const prevStructureHash = useRef<string>('');
  
  // Track current node positions via ref (avoids dependency cycle with setNodes)
  const nodesRef = useRef<Node[]>([]);
  useEffect(() => { nodesRef.current = nodes; }, [nodes]);
  
  // Compute current structure hash
  const currentStructureHash = useMemo(() => getStructureHash(project), [project]);

  // Build nodes when project STRUCTURE changes (agents/action nodes added/removed)
  useEffect(() => {
    if (!project) {
      setNodes([]);
      return;
    }
    
    // Only rebuild if structure actually changed
    if (currentStructureHash === prevStructureHash.current) {
      return;
    }
    const isFirstBuild = prevStructureHash.current === '';
    prevStructureHash.current = currentStructureHash;
    
    // Consume any pending drop position (from drag-drop onto canvas)
    const dropPosition = consumePendingDropPosition();
    
    // Capture existing node positions so we can preserve them
    const existingNodes = new Map<string, { x: number; y: number }>();
    for (const n of nodesRef.current) {
      existingNodes.set(n.id, { x: n.position.x, y: n.position.y });
    }
    
    const agentIds = Object.keys(project.agents);
    const actionNodeIds = Object.keys(project.actionNodes || {});
    
    // If no agents and no action nodes, show empty canvas (no START/END)
    if (agentIds.length === 0 && actionNodeIds.length === 0) {
      setNodes([]);
      return;
    }
    
    const allSubAgents = new Set(agentIds.flatMap(id => project.agents[id].sub_agents || []));
    const topLevelAgents = agentIds.filter(id => !allSubAgents.has(id));

    // Find nodes that connect TO START (triggers/entry points)
    const nodesConnectingToStart = project.workflow.edges
      .filter((e: WorkflowEdge) => e.to === 'START')
      .map((e: WorkflowEdge) => e.from)
      .filter((id: string) => actionNodeIds.includes(id) || topLevelAgents.includes(id));

    // Build adjacency for graph-aware layout (BFS layering)
    const allWorkflowItems = [...topLevelAgents, ...actionNodeIds].filter(
      id => !nodesConnectingToStart.includes(id)
    );
    const allNodeIds = new Set([...allWorkflowItems, 'START', 'END']);
    
    // Build forward adjacency from edges (only workflow-relevant nodes)
    const forwardEdges: Record<string, string[]> = {};
    const inDegree: Record<string, number> = {};
    for (const id of allNodeIds) {
      forwardEdges[id] = [];
      inDegree[id] = 0;
    }
    for (const e of project.workflow.edges) {
      if (nodesConnectingToStart.includes(e.from)) continue; // skip trigger→START
      if (allNodeIds.has(e.from) && allNodeIds.has(e.to)) {
        forwardEdges[e.from].push(e.to);
        inDegree[e.to] = (inDegree[e.to] || 0) + 1;
      }
    }
    
    // BFS from START to assign layers (longest path for better spread)
    const layers: Record<string, number> = {};
    const queue: string[] = ['START'];
    layers['START'] = 0;
    while (queue.length > 0) {
      const node = queue.shift()!;
      for (const next of (forwardEdges[node] || [])) {
        const newLayer = layers[node] + 1;
        if (layers[next] === undefined || newLayer > layers[next]) {
          layers[next] = newLayer;
          queue.push(next);
        }
      }
    }
    
    // Assign remaining unvisited nodes to last layer
    const maxLayer = Math.max(0, ...Object.values(layers));
    for (const id of allWorkflowItems) {
      if (layers[id] === undefined) layers[id] = maxLayer + 1;
    }
    if (layers['END'] === undefined) layers['END'] = maxLayer + 1;
    
    // Group nodes by layer
    const layerGroups: Record<number, string[]> = {};
    for (const [id, layer] of Object.entries(layers)) {
      if (id === 'START' || id === 'END') continue;
      if (!allWorkflowItems.includes(id)) continue;
      if (!layerGroups[layer]) layerGroups[layer] = [];
      layerGroups[layer].push(id);
    }
    
    const newNodes: Node[] = [];
    const nodeSpacing = 200;
    const branchSpacing = 250;
    const triggerOffset = 100;
    const startOffset = triggerOffset + (nodesConnectingToStart.length > 0 ? nodeSpacing : 0);
    const centerX = 300;

    /**
     * Resolve position for a node:
     * 1. If this is a newly added node AND we have a drop position → use drop position
     * 2. If the node already existed on canvas → preserve its current position
     * 3. Otherwise → use the computed BFS layout position (initial load / first build)
     */
    const resolvePosition = (id: string, computedPos: { x: number; y: number }): { x: number; y: number } => {
      // New node with drop position — place at cursor
      if (!existingNodes.has(id) && dropPosition) {
        return { x: dropPosition.x, y: dropPosition.y };
      }
      // Existing node — preserve position (don't rearrange on structure change)
      if (existingNodes.has(id) && !isFirstBuild) {
        return existingNodes.get(id)!;
      }
      // First build or no existing position — use computed layout
      return computedPos;
    };
    
    // Add trigger nodes (connect TO START)
    nodesConnectingToStart.forEach((id, _i) => {
      const actionNode = project.actionNodes?.[id];
      if (actionNode) {
        const computedPos = isHorizontal
          ? { x: triggerOffset, y: 200 }
          : { x: centerX, y: triggerOffset };
        const pos = resolvePosition(id, computedPos);
        const nodeType = getActionNodeType(actionNode.type);
        newNodes.push({
          id,
          type: nodeType,
          position: pos,
          data: { ...actionNode },
        });
      }
    });
    
    // Add START/END — always preserve existing positions if available
    const endLayer = layers['END'] || maxLayer + 1;
    if (allWorkflowItems.length > 0 || nodesConnectingToStart.length > 0) {
      const startComputed = isHorizontal
        ? { x: startOffset, y: 200 }
        : { x: centerX, y: startOffset };
      const endComputed = isHorizontal
        ? { x: startOffset + endLayer * nodeSpacing, y: 200 }
        : { x: centerX, y: startOffset + endLayer * nodeSpacing };
      
      newNodes.push(
        { id: 'START', position: existingNodes.get('START') || startComputed, data: {}, type: 'start' },
        { id: 'END', position: existingNodes.get('END') || endComputed, data: {}, type: 'end' },
      );
    }

    // Add all workflow nodes positioned by layer, spreading parallel nodes
    for (const [layerStr, ids] of Object.entries(layerGroups)) {
      const layer = Number(layerStr);
      const count = ids.length;
      
      ids.forEach((id, idx) => {
        const offset = (idx - (count - 1) / 2) * branchSpacing;
        const mainAxisPos = startOffset + layer * nodeSpacing;
        
        const computedPos = isHorizontal
          ? { x: mainAxisPos, y: 200 + offset }
          : { x: centerX + offset, y: mainAxisPos };
        
        const pos = resolvePosition(id, computedPos);
        
        // Check if this is an agent
        const agent = project.agents[id];
        if (agent) {
          const subAgentTools = (agent.sub_agents || []).reduce((acc, subId) => {
            acc[subId] = project.agents[subId]?.tools || [];
            return acc;
          }, {} as Record<string, string[]>);
          
          if (agent.type === 'sequential') newNodes.push({ id, type: 'sequential', position: pos, data: { label: id, subAgents: agent.sub_agents, subAgentTools } });
          else if (agent.type === 'loop') newNodes.push({ id, type: 'loop', position: pos, data: { label: id, subAgents: agent.sub_agents, subAgentTools, maxIterations: agent.max_iterations || 3 } });
          else if (agent.type === 'parallel') newNodes.push({ id, type: 'parallel', position: pos, data: { label: id, subAgents: agent.sub_agents, subAgentTools } });
          else if (agent.type === 'router') newNodes.push({ id, type: 'router', position: pos, data: { label: id, routes: agent.routes || [] } });
          else newNodes.push({ id, type: 'llm', position: pos, data: { label: id, model: agent.model, tools: agent.tools || [] } });
          return;
        }
        
        // Check if this is an action node
        const actionNode = project.actionNodes?.[id];
        if (actionNode) {
          const nodeType = getActionNodeType(actionNode.type);
          newNodes.push({
            id,
            type: nodeType,
            position: pos,
            data: { ...actionNode },
          });
        }
      });
    }

    setNodes(newNodes);
  }, [project, currentStructureHash, setNodes]);

  // Update execution state (isActive, iteration, thoughts, execution path, interrupted) WITHOUT changing positions
  useEffect(() => {
    if (!project) return;
    setNodes(nds => nds.map(n => {
      if (n.id === 'START' || n.id === 'END') {
        // v2.0: Add execution path highlighting for START/END nodes
        const isInPath = executionPath.includes(n.id);
        return {
          ...n,
          data: {
            ...n.data,
            isInExecutionPath: isInPath,
          },
          className: isInPath ? 'node-execution-path' : undefined,
        };
      }
      
      // Check if this is an action node
      const actionNode = project.actionNodes?.[n.id];
      if (actionNode) {
        const isActive = activeAgent === n.id;
        const isInPath = executionPath.includes(n.id);
        // HITL: Check if this node is interrupted
        // @see trigger-input-flow Requirement 3.3: Interrupt visual indicator
        const isInterrupted = interruptedNodeId === n.id;
        
        return {
          ...n,
          data: {
            ...n.data,
            ...actionNode, // Include latest action node config
            isActive,
            isInterrupted,
            isInExecutionPath: isInPath,
          },
          className: isInterrupted ? 'node-interrupted' : (isActive ? 'node-active' : (isInPath ? 'node-execution-path' : undefined)),
        };
      }
      
      // Handle agent nodes
      const agent = project.agents[n.id];
      if (!agent) return n;
      
      const isActive = activeAgent === n.id || (activeAgent && agent.sub_agents?.includes(activeAgent));
      const activeSub = activeAgent && agent.sub_agents?.includes(activeAgent) ? activeAgent : undefined;
      
      // v2.0: Check if node is in execution path
      // @see Requirement 10.5: Highlight execution path from start to current node
      const isInPath = executionPath.includes(n.id);
      
      // HITL: Check if this node is interrupted
      // @see trigger-input-flow Requirement 3.3: Interrupt visual indicator
      const isInterrupted = interruptedNodeId === n.id;
      
      return {
        ...n,
        data: {
          ...n.data,
          isActive,
          isInterrupted,
          activeSubAgent: activeSub,
          currentIteration: agent.type === 'loop' ? iteration : undefined,
          thought: n.type === 'llm' ? thoughts[n.id] : undefined,
          isInExecutionPath: isInPath,
        },
        // Add CSS class for execution path styling
        // HITL: Interrupted state takes precedence over active state
        className: isInterrupted ? 'node-interrupted' : (isActive ? 'node-active' : (isInPath ? 'node-execution-path' : undefined)),
      };
    }));
  }, [project, activeAgent, iteration, thoughts, executionPath, interruptedNodeId, setNodes]);

  // Rebuild edges when project edges or layout direction changes
  useEffect(() => {
    if (!project) return;
    
    // Find trigger node ID for trigger_input phase animation
    // @see trigger-input-flow Requirements 2.2, 2.3
    const triggerNodeId = Object.entries(project.actionNodes || {})
      .find(([_, node]) => node.type === 'trigger')?.[0];
    
    setEdges(project.workflow.edges.map((e: WorkflowEdge, i: number) => {
      // Edge animation is driven by the execution path, which tracks the actual
      // sequence of node executions (set → transform → agent, etc.).
      // The flowPhase only controls the trigger→START and agent→END animations.
      // @see trigger-input-flow Requirements 2.2, 2.3
      const isTriggerToStart = flowPhase === 'trigger_input' && e.from === triggerNodeId && e.to === 'START';
      
      // v2.0: Check if edge is in execution path (completed segments)
      // @see Requirement 10.3, 10.5: Highlight execution path
      const sourceIndex = executionPath.indexOf(e.from);
      const targetIndex = executionPath.indexOf(e.to);
      const isInPath = sourceIndex !== -1 && targetIndex !== -1 && targetIndex === sourceIndex + 1;
      
      // Edge is animated if it leads TO the currently active node from a node
      // in the execution path. The activeAgent is managed by a queue with 300ms
      // display time per node, so each node gets visible edge animation even if
      // the underlying execution is faster.
      // Note: We no longer check flowPhase !== 'output' here because during loop
      // iterations, the LLM's streaming text sets flowPhase to 'output' almost
      // immediately, which would suppress edge animations for action nodes that
      // execute between LLM calls. The queue timing handles visibility instead.
      const isLeadingToActive = isExecuting && activeAgent && e.to === activeAgent 
        && executionPath.includes(e.from);
      // Animate agent→END when we're in output phase (data flowing out)
      const isAgentToEnd = flowPhase === 'output' && e.to === 'END' && executionPath.includes(e.from);
      
      const animated = isTriggerToStart || isLeadingToActive || isAgentToEnd;
      
      // Determine source and target handles
      // For multi-port nodes (Switch, Merge), use the port names from edge
      // For action nodes, use layout-aware handles (left/right for horizontal, top/bottom for vertical)
      // For agent nodes, use layout-based defaults
      const isSourceActionNode = project.actionNodes?.[e.from] !== undefined;
      const isTargetActionNode = project.actionNodes?.[e.to] !== undefined;
      const isSourceStartEnd = e.from === 'START' || e.from === 'END';
      const isTargetStartEnd = e.to === 'START' || e.to === 'END';
      
      // Default handles based on node type and layout direction
      let defaultSourceHandle: string;
      let defaultTargetHandle: string;
      
      if (isSourceActionNode) {
        // Action nodes use output-0 (position adapts based on layout in component)
        defaultSourceHandle = 'output-0';
      } else if (isSourceStartEnd) {
        // START/END nodes have named handles
        defaultSourceHandle = isHorizontal ? 'right' : 'bottom';
      } else {
        // Agent nodes
        defaultSourceHandle = isHorizontal ? 'right' : 'bottom';
      }
      
      if (isTargetActionNode) {
        // Action nodes use input-0 (position adapts based on layout in component)
        defaultTargetHandle = 'input-0';
      } else if (isTargetStartEnd) {
        // START/END nodes have named handles
        defaultTargetHandle = isHorizontal ? 'left' : 'top';
      } else {
        // Agent nodes
        defaultTargetHandle = isHorizontal ? 'left' : 'top';
      }
      
      const isInPathNotAnimated = isInPath && !animated;
      
      return { 
        id: `e${i}-${layoutDirection}${animated ? '-anim' : isInPathNotAnimated ? '-path' : ''}`,
        source: e.from, 
        target: e.to, 
        // Use dataflow edge type when overlay is enabled, otherwise animated
        type: showDataFlowOverlay ? 'dataflow' : 'animated', 
        data: { 
          animated,
          // v2.0: Data flow overlay data
          stateKeys: stateKeys?.get(e.from) || [],
          showOverlay: showDataFlowOverlay,
          highlightedKey,
          onKeyHover,
          // v2.0: Execution path data — completed path segments (not currently animated)
          isExecutionPath: isInPathNotAnimated,
        },
        // Use port-specific handles if specified, otherwise use defaults
        sourceHandle: e.fromPort || defaultSourceHandle,
        targetHandle: e.toPort || defaultTargetHandle,
      };
    }));
  }, [project?.workflow.edges, flowPhase, activeAgent, setEdges, layoutDirection, isHorizontal, stateKeys, showDataFlowOverlay, highlightedKey, onKeyHover, executionPath, isExecuting]);

  return { nodes, edges, setNodes, setEdges, onNodesChange, onEdgesChange };
}

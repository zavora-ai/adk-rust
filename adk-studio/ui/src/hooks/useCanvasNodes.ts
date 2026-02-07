import { useEffect, useRef, useMemo } from 'react';
import { Node, Edge, useNodesState, useEdgesState } from '@xyflow/react';
import type { Project, Edge as WorkflowEdge } from '../types/project';
import type { ActionNodeConfig } from '../types/actionNodes';
import { useStore } from '../store';

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
  const toolsHash = Object.entries(project.agents)
    .map(([id, a]) => `${id}:${(a.tools || []).join('+')}`)
    .sort()
    .join('|');
  const edgesHash = project.workflow.edges
    .map(e => `${e.from}->${e.to}`)
    .sort()
    .join(',');
  
  return `${agentKeys}|${actionNodeKeys}|${toolsHash}|${edgesHash}`;
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
    prevStructureHash.current = currentStructureHash;
    
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

    // Sort workflow items that come AFTER START (agents and action nodes)
    const allWorkflowItems = [...topLevelAgents, ...actionNodeIds].filter(
      id => !nodesConnectingToStart.includes(id)
    );
    const sortedWorkflowItems: string[] = [];
    let current = 'START';
    const visited = new Set<string>();
    
    // Follow edges from START to END to determine order
    while (sortedWorkflowItems.length < allWorkflowItems.length) {
      const nextEdge = project.workflow.edges.find((e: WorkflowEdge) => 
        e.from === current && 
        e.to !== 'END' && 
        allWorkflowItems.includes(e.to) && 
        !visited.has(e.to)
      );
      
      if (nextEdge) {
        sortedWorkflowItems.push(nextEdge.to);
        visited.add(nextEdge.to);
        current = nextEdge.to;
      } else {
        // Try to find any unvisited item connected in the workflow
        const anyEdge = project.workflow.edges.find((e: WorkflowEdge) => 
          allWorkflowItems.includes(e.to) && !visited.has(e.to)
        );
        if (anyEdge) {
          sortedWorkflowItems.push(anyEdge.to);
          visited.add(anyEdge.to);
          current = anyEdge.to;
        } else {
          break;
        }
      }
    }
    
    // Add any remaining items not in workflow
    allWorkflowItems.forEach(id => { 
      if (!sortedWorkflowItems.includes(id)) sortedWorkflowItems.push(id); 
    });

    const newNodes: Node[] = [];
    
    // Calculate positions based on layout direction
    // For horizontal (LR): Trigger → START → Agents → END
    // For vertical (TB): Trigger above START, then agents below
    const nodeSpacing = 200;
    const triggerOffset = 100;  // Position for trigger nodes
    const startOffset = triggerOffset + (nodesConnectingToStart.length > 0 ? nodeSpacing : 0);
    
    // Total items after START
    const itemsAfterStart = sortedWorkflowItems.length;
    
    // Add trigger nodes (connect TO START)
    nodesConnectingToStart.forEach((id, _i) => {
      const actionNode = project.actionNodes?.[id];
      if (actionNode) {
        const pos = isHorizontal
          ? { x: triggerOffset, y: 200 }  // Left of START
          : { x: 300, y: triggerOffset }; // Above START
        const nodeType = getActionNodeType(actionNode.type);
        newNodes.push({
          id,
          type: nodeType,
          position: pos,
          data: { ...actionNode },
        });
      }
    });
    
    // Add START/END
    if (sortedWorkflowItems.length > 0 || nodesConnectingToStart.length > 0) {
      if (isHorizontal) {
        // Horizontal layout: Trigger → START → Agents → END
        const endX = startOffset + (itemsAfterStart + 1) * nodeSpacing;
        newNodes.push(
          { id: 'START', position: { x: startOffset, y: 200 }, data: {}, type: 'start' },
          { id: 'END', position: { x: endX, y: 200 }, data: {}, type: 'end' },
        );
      } else {
        // Vertical layout: Trigger → START → Agents → END
        const endY = startOffset + (itemsAfterStart + 1) * nodeSpacing;
        newNodes.push(
          { id: 'START', position: { x: 300, y: startOffset }, data: {}, type: 'start' },
          { id: 'END', position: { x: 300, y: endY }, data: {}, type: 'end' },
        );
      }
    }

    // Add all workflow nodes (agents and action nodes) in workflow order
    sortedWorkflowItems.forEach((id, i) => {
      // Position based on layout direction
      const pos = isHorizontal
        ? { x: startOffset + (i + 1) * nodeSpacing, y: 200 }  // Horizontal: spread along X
        : { x: 300, y: startOffset + (i + 1) * nodeSpacing }; // Vertical: spread along Y
      
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
      
      // Edge is animated if it leads TO the currently active node from a node in the path,
      // but NOT during 'output' phase — once the agent is generating output, the input
      // edge is "done" and should show as completed (green), not still flowing.
      const isLeadingToActive = isExecuting && activeAgent && e.to === activeAgent 
        && executionPath.includes(e.from) && flowPhase !== 'output';
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
      
      return { 
        id: `e${i}-${layoutDirection}`,
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
          isExecutionPath: isInPath && !animated,
        },
        // Use port-specific handles if specified, otherwise use defaults
        sourceHandle: e.fromPort || defaultSourceHandle,
        targetHandle: e.toPort || defaultTargetHandle,
      };
    }));
  }, [project?.workflow.edges, flowPhase, activeAgent, setEdges, layoutDirection, isHorizontal, stateKeys, showDataFlowOverlay, highlightedKey, onKeyHover, executionPath, isExecuting]);

  return { nodes, edges, setNodes, setEdges, onNodesChange, onEdgesChange };
}

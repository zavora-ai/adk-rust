import { useCallback, useState, useRef, useMemo, useEffect, DragEvent } from 'react';
import { ReactFlow, Background, Controls, MiniMap, Node, Edge, Connection } from '@xyflow/react';
import '@xyflow/react/dist/style.css';
import { useStore } from '../../store';
import { TestConsole, BuildStatus } from '../Console/TestConsole';
import { MenuBar } from '../MenuBar';
import { nodeTypes } from '../Nodes';
import { edgeTypes } from '../Edges';
import { AgentPalette, ToolPalette, PropertiesPanel, ToolConfigPanel, StateInspector, ActionPalette, ActionPropertiesPanel } from '../Panels';
import { CodeModal, BuildModal, CodeEditorModal, NewProjectModal, SettingsModal } from '../Overlays';
import { TimelineView } from '../Timeline';
import { CanvasToolbar } from './CanvasToolbar';
import { api } from '../../api/client';
import { useKeyboardShortcuts } from '../../hooks/useKeyboardShortcuts';
import { useLayout } from '../../hooks/useLayout';
import { useCanvasNodes } from '../../hooks/useCanvasNodes';
import { useAgentActions } from '../../hooks/useAgentActions';
import { useCanvasState } from '../../hooks/useCanvasState';
import { useBuild } from '../../hooks/useBuild';
import { useTheme } from '../../hooks/useTheme';
import { useExecutionPath } from '../../hooks/useExecutionPath';
import { useUndoRedo, useUndoRedoStore } from '../../hooks/useUndoRedo';
import type { FunctionToolConfig, AgentSchema, ToolConfig, Edge as ProjectEdge } from '../../types/project';
import type { ActionNodeType } from '../../types/actionNodes';
import { createDefaultStandardProperties } from '../../types/standardProperties';
import { DEFAULT_MANUAL_TRIGGER_CONFIG } from '../../types/actionNodes';
import { TEMPLATES } from '../MenuBar/templates';
import { validateConnection } from '../../utils/connectionValidation';

/**
 * Flow phase for edge animations.
 * - 'idle': No activity
 * - 'trigger_input': User submitting input to trigger (animates trigger→START)
 * - 'input': Data flowing from START to agents
 * - 'output': Agent generating response
 * - 'interrupted': Waiting for HITL response
 * @see trigger-input-flow Requirements 2.2, 2.3, 3.1, 3.2
 */
type FlowPhase = 'idle' | 'trigger_input' | 'input' | 'output' | 'interrupted';

export function Canvas() {
  // Store state
  const { 
    currentProject, 
    openProject, 
    closeProject, 
    saveProject, 
    selectNode, 
    selectedNodeId, 
    updateAgent: storeUpdateAgent, 
    renameAgent, 
    addEdge: addProjectEdge, 
    removeEdge: removeProjectEdge, 
    addToolToAgent, 
    removeToolFromAgent, 
    addSubAgentToContainer, 
    selectedToolId, 
    selectTool, 
    updateToolConfig: storeUpdateToolConfig, 
    addAgent,
    snapToGrid,
    gridSize,
    // v2.0: Action node state
    addActionNode,
    removeActionNode,
    selectedActionNodeId,
    selectActionNode,
    // v2.0: Data flow overlay state
    showDataFlowOverlay,
    setShowDataFlowOverlay,
    // v2.0: Debug mode state
    debugMode,
    setDebugMode,
    // Settings
    updateProjectMeta,
    updateProjectSettings,
  } = useStore();

  // Callback to persist UI settings changes to project
  const handleUISettingChange = useCallback((key: string, value: boolean) => {
    updateProjectSettings({ [key]: value });
  }, [updateProjectSettings]);

  // Canvas UI state - pass project settings to initialize from saved preferences
  const { showConsole, toggleConsole, showMinimap, toggleMinimap, showTimeline, toggleTimeline: _toggleTimeline } = useCanvasState(currentProject?.settings, handleUISettingChange);
  
  // Check if project can be built (has agents OR action nodes, and edges)
  const canBuild = useCallback(() => {
    if (!currentProject) return false;
    const agentCount = Object.keys(currentProject.agents).length;
    const actionNodeCount = Object.keys(currentProject.actionNodes || {}).length;
    const edgeCount = currentProject.workflow.edges.length;
    // Allow build if we have either agents OR action nodes, plus edges
    return (agentCount > 0 || actionNodeCount > 0) && edgeCount > 0;
  }, [currentProject]);

  // Build state - pass project settings for autobuild configuration
  const { 
    building, 
    buildOutput, 
    builtBinaryPath, 
    compiledCode, 
    autobuildEnabled,
    isAutobuild,
    build: handleBuild, 
    compile: handleCompile, 
    clearBuildOutput, 
    clearCompiledCode,
    invalidateBuild,
    toggleAutobuild,
    showBuildProgress,
  } = useBuild(
    currentProject?.id, 
    currentProject?.settings?.autobuildTriggers,
    currentProject?.settings?.autobuildEnabled,
    canBuild
  );

  // v2.0: Derive build status for console summary (Requirement 13.2)
  const buildStatus: BuildStatus = building 
    ? 'building' 
    : buildOutput?.success 
      ? 'success' 
      : buildOutput && !buildOutput.success 
        ? 'error' 
        : builtBinaryPath 
          ? 'success' 
          : 'none';
  
  // v2.0: Console collapse state
  const [consoleCollapsed, setConsoleCollapsed] = useState(false);
  
  // Auto-send prompt state (for Run button to trigger workflow with default prompt)
  const [autoSendPrompt, setAutoSendPrompt] = useState<string | null>(null);
  
  // Cancel function ref (exposed by TestConsole for Stop button)
  const cancelFnRef = useRef<(() => void) | null>(null);

  // Execution state (local for now, will be moved to useExecution in later tasks)
  const [flowPhase, setFlowPhase] = useState<FlowPhase>('idle');
  const [activeAgent, setActiveAgent] = useState<string | null>(null);
  const [iteration, setIteration] = useState(0);
  const [thoughts, setThoughts] = useState<Record<string, string>>({});
  
  // Execution path tracking (v2.0) - must be before callbacks that use it
  // @see Requirements 10.3, 10.5: Execution path highlighting
  const executionPath = useExecutionPath();
  
  // v2.0: Wrapper for flow phase that also updates execution path
  // @see Requirements 10.3, 10.5: Execution path highlighting
  const handleFlowPhase = useCallback((phase: FlowPhase) => {
    setFlowPhase(phase);
    if (phase === 'input') {
      // Starting new execution
      executionPath.startExecution();
    } else if (phase === 'idle' && executionPath.isExecuting) {
      // Execution completed
      executionPath.completeExecution();
    }
  }, [executionPath]);
  
  // v2.0: Wrapper for active agent that also updates execution path
  const handleActiveAgent = useCallback((agent: string | null) => {
    setActiveAgent(agent);
    if (agent && executionPath.isExecuting && !executionPath.path.includes(agent)) {
      executionPath.moveToNode(agent);
    }
  }, [executionPath]);
  
  // Modal state
  const [showCodeEditor, setShowCodeEditor] = useState(false);
  const [showNewProjectModal, setShowNewProjectModal] = useState(false);
  const [showSettingsModal, setShowSettingsModal] = useState(false);
  
  // Timeline state (v2.0)
  const [timelineCollapsed, setTimelineCollapsed] = useState(false);
  const [snapshots, setSnapshots] = useState<import('../../types/execution').StateSnapshot[]>([]);
  const [currentSnapshotIndex, setCurrentSnapshotIndex] = useState(-1);
  const [scrubToFn, setScrubToFn] = useState<((index: number) => void) | null>(null);
  
  // State Inspector visibility (v2.0)
  const [showStateInspector, setShowStateInspector] = useState(true);
  
  // HITL: Interrupted node ID for visual indicator (v2.0)
  // @see trigger-input-flow Requirement 3.3: Interrupt visual indicator
  const [interruptedNodeId, setInterruptedNodeId] = useState<string | null>(null);
  
  // Data Flow Overlay state (v2.0)
  // @see Requirements 3.1-3.9: Data flow overlays
  // Note: showDataFlowOverlay is now managed by the store for persistence
  const [stateKeys, setStateKeys] = useState<Map<string, string[]>>(new Map());
  const [highlightedKey, setHighlightedKey] = useState<string | null>(null);
  
  // Handler for state key hover (for highlighting related edges)
  // @see Requirements 3.8: Highlight all edges using same key on hover
  const handleKeyHover = useCallback((key: string | null) => {
    setHighlightedKey(key);
  }, []);
  
  // Handler for toggling data flow overlay
  // @see Requirements 3.4: Toggle to show/hide data flow overlays
  const handleToggleDataFlowOverlay = useCallback(() => {
    setShowDataFlowOverlay(!showDataFlowOverlay);
  }, [showDataFlowOverlay, setShowDataFlowOverlay]);
  
  // HITL: Handler for interrupt state changes from TestConsole
  // @see trigger-input-flow Requirement 3.3: Interrupt visual indicator
  const handleInterruptChange = useCallback((interrupt: import('../../types/execution').InterruptData | null) => {
    setInterruptedNodeId(interrupt?.nodeId || null);
  }, []);
  
  // Handler for receiving snapshots and state keys from TestConsole
  const handleSnapshotsChange = useCallback((
    newSnapshots: import('../../types/execution').StateSnapshot[], 
    newIndex: number, 
    scrubTo: (index: number) => void,
    newStateKeys?: Map<string, string[]>
  ) => {
    setSnapshots(newSnapshots);
    setCurrentSnapshotIndex(newIndex);
    setScrubToFn(() => scrubTo);
    if (newStateKeys) {
      setStateKeys(newStateKeys);
    }
    
    // v2.0: Update execution path based on snapshots
    // @see Requirements 10.3, 10.5: Execution path highlighting
    if (newSnapshots.length > 0) {
      // Reset and rebuild path from snapshots
      executionPath.resetPath();
      executionPath.startExecution();
      newSnapshots.forEach(s => {
        executionPath.moveToNode(s.nodeId);
      });
      
      // If we're at the last snapshot and it's complete, mark execution as done
      const lastSnapshot = newSnapshots[newSnapshots.length - 1];
      if (lastSnapshot && lastSnapshot.status === 'success' && newIndex === newSnapshots.length - 1) {
        // Don't complete yet - wait for actual completion
      }
    }
  }, [executionPath]);

  // Current and previous snapshots for StateInspector (v2.0)
  // @see Requirements 4.5, 5.4: Update inspector when timeline position changes
  const currentSnapshot = useMemo(() => {
    if (currentSnapshotIndex < 0 || currentSnapshotIndex >= snapshots.length) {
      return null;
    }
    return snapshots[currentSnapshotIndex];
  }, [snapshots, currentSnapshotIndex]);

  const previousSnapshot = useMemo(() => {
    const prevIndex = currentSnapshotIndex - 1;
    if (prevIndex < 0 || prevIndex >= snapshots.length) {
      return null;
    }
    return snapshots[prevIndex];
  }, [snapshots, currentSnapshotIndex]);

  // Handler for state inspector history selection
  const handleStateHistorySelect = useCallback((index: number) => {
    if (scrubToFn) {
      scrubToFn(index);
    }
  }, [scrubToFn]);

  // Canvas hooks
  const { nodes, edges, onNodesChange, onEdgesChange } = useCanvasNodes(currentProject, { 
    activeAgent, 
    iteration, 
    flowPhase, 
    thoughts,
    // v2.0: Data flow overlay support
    stateKeys,
    showDataFlowOverlay,
    highlightedKey,
    onKeyHover: handleKeyHover,
    // v2.0: Execution path highlighting
    // @see Requirements 10.3, 10.5
    executionPath: executionPath.path,
    isExecuting: executionPath.isExecuting,
    // HITL: Interrupted node ID for visual indicator
    // @see trigger-input-flow Requirement 3.3
    interruptedNodeId,
  });
  const { applyLayout, fitToView, zoomIn, zoomOut } = useLayout();
  const { createAgent, duplicateAgent, removeAgent } = useAgentActions();
  
  // v2.0: Undo/Redo MVP
  // @see Requirements 11.5, 11.6: Undo/Redo support
  const { clearHistory: clearUndoHistory } = useUndoRedoStore();
  
  // Helper to get edges connected to a node
  const getEdgesForNode = useCallback((nodeId: string) => {
    if (!currentProject) return [];
    return currentProject.workflow.edges.filter(
      (e) => e.from === nodeId || e.to === nodeId
    );
  }, [currentProject]);
  
  // Helper to get all edges
  const getAllEdges = useCallback(() => {
    return currentProject?.workflow.edges || [];
  }, [currentProject]);
  
  // Helper to get agent by ID
  const getAgent = useCallback((nodeId: string) => {
    return currentProject?.agents[nodeId];
  }, [currentProject]);
  
  // Helper to set all edges (for undo/redo)
  const setEdges = useCallback((edges: ProjectEdge[]) => {
    useStore.getState().setEdges(edges);
  }, []);
  
  // Undo/Redo hook with handlers
  const undoRedo = useUndoRedo({
    onAddNode: (nodeId, agent) => {
      addAgent(nodeId, agent);
    },
    onRemoveNode: (nodeId) => {
      // Use store's removeAgent directly to avoid recording again
      useStore.getState().removeAgent(nodeId);
    },
    onAddEdge: addProjectEdge,
    onRemoveEdge: removeProjectEdge,
    onSetEdges: setEdges,
    getAgent,
    getEdgesForNode,
    getAllEdges,
  });
  
  // Wrapped createAgent that records for undo
  const createAgentWithUndo = useCallback((agentType?: string) => {
    // Capture the complete edge state BEFORE creating the agent
    // Read from store directly to avoid stale closures
    const edgesBefore = [...(useStore.getState().currentProject?.workflow.edges || [])];
    
    // Create the agent
    createAgent(agentType);
    
    // After creation, find the new agent and record it
    // We need to defer this to get the updated state
    setTimeout(() => {
      const state = useStore.getState();
      const project = state.currentProject;
      if (!project) return;
      
      // Find the newly added edges (edges that exist now but didn't before)
      const newEdges = project.workflow.edges.filter(
        (e) => !edgesBefore.some((eb) => eb.from === e.from && eb.to === e.to)
      );
      
      // The new agent is the source of edges to END or target of edges from START
      const newAgentId = newEdges.find((e) => e.to === 'END')?.from;
      if (newAgentId && project.agents[newAgentId]) {
        // Record with both the new edges AND the complete previous edge state
        undoRedo.recordAddNode(newAgentId, project.agents[newAgentId], newEdges, edgesBefore);
      }
    }, 0);
  }, [createAgent, undoRedo]);
  
  // Wrapped removeAgent that records for undo and applies layout
  const removeAgentWithUndo = useCallback((nodeId: string) => {
    if (nodeId === 'START' || nodeId === 'END') return;
    
    // Capture the complete edge state BEFORE removing
    // Read from store directly to avoid stale closures
    const edgesBefore = [...(useStore.getState().currentProject?.workflow.edges || [])];
    
    // Record before removing (with complete edge state)
    undoRedo.recordDeleteNode(nodeId, edgesBefore);
    
    // Then remove
    removeAgent(nodeId);
    
    // Apply layout after deletion to maintain current layout direction
    invalidateBuild('onAgentDelete');
    setTimeout(() => applyLayout(), 100);
  }, [removeAgent, undoRedo, invalidateBuild, applyLayout]);
  
  // Wrapped removeActionNode that also applies layout after deletion
  // This ensures the layout direction is maintained after structure changes
  const removeActionNodeWithLayout = useCallback((nodeId: string) => {
    removeActionNode(nodeId);
    invalidateBuild('onAgentDelete'); // Action nodes use same trigger as agents
    setTimeout(() => applyLayout(), 100);
  }, [removeActionNode, invalidateBuild, applyLayout]);
  
  // Clear undo history when project changes
  const prevProjectIdRef = useRef<string | null>(null);
  const hasAppliedInitialLayout = useRef<string | null>(null);
  
  if (currentProject?.id !== prevProjectIdRef.current) {
    prevProjectIdRef.current = currentProject?.id || null;
    hasAppliedInitialLayout.current = null; // Reset layout flag for new project
    clearUndoHistory();
  }
  
  // Apply layout when a new project is opened (after nodes are rendered)
  // Also triggers when nodes are added to a project that didn't have any
  const nodeCount = (currentProject ? Object.keys(currentProject.agents).length + Object.keys(currentProject.actionNodes || {}).length : 0);
  
  useEffect(() => {
    if (!currentProject) return;
    if (nodeCount === 0) return;
    
    // Only apply initial layout once per project
    if (hasAppliedInitialLayout.current === currentProject.id) return;
    
    // Delay to ensure nodes are rendered by ReactFlow
    // Use multiple attempts to handle race conditions
    const timer1 = setTimeout(() => {
      applyLayout();
    }, 100);
    
    const timer2 = setTimeout(() => {
      applyLayout();
      hasAppliedInitialLayout.current = currentProject.id;
    }, 300);
    
    return () => {
      clearTimeout(timer1);
      clearTimeout(timer2);
    };
  }, [currentProject, nodeCount, applyLayout]);

  // Thought bubble handler
  const handleThought = useCallback((agent: string, thought: string | null) => {
    setThoughts(prev => thought ? { ...prev, [agent]: thought } : Object.fromEntries(Object.entries(prev).filter(([k]) => k !== agent)));
  }, []);

  // Debounced save
  const saveTimerRef = useRef<ReturnType<typeof setTimeout> | null>(null);
  const debouncedSave = useCallback(() => {
    if (saveTimerRef.current) clearTimeout(saveTimerRef.current);
    saveTimerRef.current = setTimeout(() => saveProject(), 500);
  }, [saveProject]);

  // Agent update with save and build invalidation
  const updateAgent = useCallback((id: string, updates: Partial<AgentSchema>) => {
    storeUpdateAgent(id, updates);
    invalidateBuild('onAgentUpdate');
    debouncedSave();
  }, [storeUpdateAgent, invalidateBuild, debouncedSave]);

  // Tool config update with save and build invalidation
  const updateToolConfig = useCallback((toolId: string, config: ToolConfig) => {
    storeUpdateToolConfig(toolId, config);
    invalidateBuild('onToolUpdate');
    debouncedSave();
  }, [storeUpdateToolConfig, invalidateBuild, debouncedSave]);

  // Keyboard shortcuts
  // @see Requirements 11.1-11.10: Keyboard Shortcuts
  useKeyboardShortcuts({
    selectedNodeId, 
    selectedToolId,
    selectedActionNodeId,
    onDeleteNode: removeAgentWithUndo,
    onDeleteActionNode: removeActionNodeWithLayout,
    onDeleteTool: removeToolFromAgent,
    onDuplicateNode: duplicateAgent,
    onSelectNode: selectNode,
    onSelectActionNode: selectActionNode,
    onSelectTool: selectTool,
    onAutoLayout: applyLayout,
    onFitView: fitToView,
    onZoomIn: zoomIn,
    onZoomOut: zoomOut,
    // v2.0: Undo/Redo MVP (Task 24)
    // @see Requirements 11.5, 11.6
    onUndo: undoRedo.undo,
    onRedo: undoRedo.redo,
  });

  // Drag and drop handlers
  const onDragStart = (e: DragEvent, type: string) => { 
    e.dataTransfer.setData('application/reactflow', type); 
    e.dataTransfer.effectAllowed = 'move'; 
  };
  
  // Action node drag start handler
  const onActionDragStart = (e: DragEvent, type: ActionNodeType) => {
    e.dataTransfer.setData('application/actionnode', type);
    e.dataTransfer.effectAllowed = 'move';
  };
  
  // Create action node handler
  // Action nodes integrate into the workflow the same way agents do:
  // - If first item on canvas, connect START -> node -> END
  // - If other items exist, insert before END (remove edge to END, connect previous -> new -> END)
  const createActionNode = useCallback((type: ActionNodeType) => {
    if (!currentProject) return;
    
    const id = `${type}_${Date.now()}`;
    const name = type.charAt(0).toUpperCase() + type.slice(1);
    const baseProps = createDefaultStandardProperties(id, name, `${type}Result`);
    
    // Create node config based on type
    let nodeConfig: import('../../types/actionNodes').ActionNodeConfig;
    
    switch (type) {
      case 'trigger':
        nodeConfig = { ...baseProps, type: 'trigger', triggerType: 'manual' };
        break;
      case 'http':
        nodeConfig = { 
          ...baseProps, 
          type: 'http', 
          method: 'GET', 
          url: 'https://api.example.com', 
          auth: { type: 'none' },
          headers: {},
          body: { type: 'none' },
          response: { type: 'json' },
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
          ...baseProps, 
          type: 'loop', 
          loopType: 'forEach',
          forEach: { sourceArray: '', itemVar: 'item', indexVar: 'index' },
          parallel: { enabled: false },
          results: { collect: true },
        };
        break;
      case 'merge':
        nodeConfig = { 
          ...baseProps, 
          type: 'merge', 
          mode: 'wait_all', 
          combineStrategy: 'array',
          timeout: { enabled: false, ms: 30000, behavior: 'error' },
        };
        break;
      case 'wait':
        nodeConfig = { 
          ...baseProps, 
          type: 'wait', 
          waitType: 'fixed',
          fixed: { duration: 1000, unit: 'ms' },
        };
        break;
      case 'code':
        nodeConfig = { 
          ...baseProps, 
          type: 'code', 
          language: 'javascript',
          code: '// Your code here\nreturn input;',
          sandbox: { networkAccess: false, fileSystemAccess: false, memoryLimit: 128, timeLimit: 5000 },
        };
        break;
      case 'database':
        nodeConfig = { 
          ...baseProps, 
          type: 'database', 
          dbType: 'postgresql',
          connection: { connectionString: '' },
        };
        break;
      case 'email':
        nodeConfig = { 
          ...baseProps, 
          type: 'email', 
          mode: 'send',
          smtp: {
            host: 'smtp.example.com',
            port: 587,
            secure: true,
            username: '',
            password: '',
            fromEmail: '',
          },
          recipients: { to: '' },
          content: { subject: '', body: '', bodyType: 'text' },
          attachments: [],
        };
        break;
      default:
        return;
    }
    
    // Add the action node
    addActionNode(id, nodeConfig);
    
    // Special handling for trigger nodes:
    // - Only one trigger allowed per workflow
    // - Trigger connects TO START (not from START like other nodes)
    // - Visual flow: [Trigger] → START → agents → END
    if (type === 'trigger') {
      // Check if a trigger already exists
      const existingTrigger = Object.values(currentProject.actionNodes || {}).find(
        (node) => node.type === 'trigger'
      );
      if (existingTrigger && existingTrigger.id !== id) {
        // Remove the newly added trigger - only one allowed
        useStore.getState().removeActionNode(id);
        alert('Only one trigger node is allowed per workflow. Remove the existing trigger first.');
        return;
      }
      
      // Connect trigger TO START (trigger is the entry point)
      addProjectEdge(id, 'START');
    } else {
      // Connect to workflow edges (same logic as agents)
      // Find edge going to END and insert this node before it
      const edgeToEnd = currentProject.workflow.edges.find(e => e.to === 'END');
      if (edgeToEnd) {
        // Remove the existing edge to END
        removeProjectEdge(edgeToEnd.from, 'END');
        // Connect previous node to this new node
        addProjectEdge(edgeToEnd.from, id);
      } else {
        // No existing edges to END, connect from START
        addProjectEdge('START', id);
      }
      // Connect this node to END
      addProjectEdge(id, 'END');
    }
    
    selectActionNode(id);
    invalidateBuild('onAgentAdd'); // Action nodes use same trigger as agents
    setTimeout(() => applyLayout(), 100);
  }, [currentProject, addActionNode, addProjectEdge, removeProjectEdge, selectActionNode, applyLayout, invalidateBuild]);
  
  const onDragOver = useCallback((e: DragEvent) => { 
    e.preventDefault(); 
    e.dataTransfer.dropEffect = e.dataTransfer.types.includes('text/plain') ? 'copy' : 'move'; 
  }, []);
  
  const onDrop = useCallback((e: DragEvent) => {
    e.preventDefault();
    const toolData = e.dataTransfer.getData('text/plain');
    if (toolData.startsWith('tool:') && selectedNodeId && currentProject?.agents[selectedNodeId]) {
      addToolToAgent(selectedNodeId, toolData.slice(5));
      invalidateBuild('onToolAdd'); // Trigger autobuild when tool is added
      return;
    }
    
    // Handle action node drop
    const actionType = e.dataTransfer.getData('application/actionnode');
    if (actionType) {
      createActionNode(actionType as ActionNodeType);
      return;
    }
    
    const type = e.dataTransfer.getData('application/reactflow');
    if (type) {
      createAgentWithUndo(type);
      invalidateBuild('onAgentAdd'); // Trigger autobuild when agent is added
      // Apply layout after adding node (only in fixed mode or always for initial setup)
      setTimeout(() => applyLayout(), 100);
    }
  }, [createAgentWithUndo, createActionNode, selectedNodeId, currentProject, addToolToAgent, applyLayout, invalidateBuild]);

  // Connection handlers
  // @see Requirement 12.3: Edge connections between action nodes and agents
  const onConnect = useCallback((p: Connection) => {
    if (p.source && p.target && currentProject) {
      // Validate the connection
      const validation = validateConnection(
        p.source,
        p.target,
        currentProject.agents,
        currentProject.actionNodes || {},
        currentProject.workflow.edges
      );
      
      if (!validation.valid) {
        // Could show a toast notification here
        console.warn('Invalid connection:', validation.reason);
        return;
      }
      
      addProjectEdge(p.source, p.target);
      invalidateBuild('onEdgeAdd'); // Trigger autobuild when edge is added
    }
  }, [addProjectEdge, invalidateBuild, currentProject]);
  const onEdgesDelete = useCallback((eds: Edge[]) => {
    eds.forEach(e => removeProjectEdge(e.source, e.target));
    if (eds.length > 0) invalidateBuild('onEdgeDelete'); // Trigger autobuild when edges are deleted
  }, [removeProjectEdge, invalidateBuild]);
  const onNodesDelete = useCallback((nds: Node[]) => {
    // Track if we need to apply layout (only if action nodes are deleted without agents)
    let hasActionNodeDeletion = false;
    let hasAgentDeletion = false;
    
    nds.forEach(n => {
      if (n.id === 'START' || n.id === 'END') return;
      
      // Check if it's an action node (type starts with 'action_')
      if (n.type?.startsWith('action_')) {
        removeActionNode(n.id);
        hasActionNodeDeletion = true;
      } else {
        // removeAgentWithUndo already calls applyLayout
        removeAgentWithUndo(n.id);
        hasAgentDeletion = true;
      }
    });
    
    // Only apply layout for action node deletions if no agent was deleted
    // (agent deletion already triggers applyLayout via removeAgentWithUndo)
    if (hasActionNodeDeletion && !hasAgentDeletion) {
      invalidateBuild('onAgentDelete');
      setTimeout(() => applyLayout(), 100);
    }
  }, [removeAgentWithUndo, removeActionNode, invalidateBuild, applyLayout]);
  const onEdgeDoubleClick = useCallback((_: React.MouseEvent, e: Edge) => {
    removeProjectEdge(e.source, e.target);
    invalidateBuild('onEdgeDelete'); // Trigger autobuild when edge is deleted
  }, [removeProjectEdge, invalidateBuild]);
  const onNodeClick = useCallback((_: React.MouseEvent, n: Node) => {
    // Skip START and END nodes
    if (n.id === 'START' || n.id === 'END') {
      selectNode(null);
      selectActionNode(null);
      return;
    }
    
    // Check if this is an action node (type starts with 'action_')
    if (n.type?.startsWith('action_')) {
      selectActionNode(n.id);
      selectNode(null); // Deselect any agent node
    } else {
      selectNode(n.id);
      selectActionNode(null); // Deselect any action node
    }
  }, [selectNode, selectActionNode]);
  const onPaneClick = useCallback(() => {
    selectNode(null);
    selectActionNode(null);
  }, [selectNode, selectActionNode]);

  // Tool add handler
  const handleAddTool = useCallback((type: string) => {
    if (!selectedNodeId) return;
    addToolToAgent(selectedNodeId, type);
    const tools = currentProject?.agents[selectedNodeId]?.tools || [];
    const isMulti = type === 'function' || type === 'mcp';
    const newId = isMulti ? `${selectedNodeId}_${type}_${tools.filter(t => t.startsWith(type)).length + 1}` : `${selectedNodeId}_${type}`;
    setTimeout(() => selectTool(newId), 0);
  }, [selectedNodeId, currentProject, addToolToAgent, selectTool]);

  // Early return if no project
  if (!currentProject) return null;
  
  // Derived state
  const selectedAgent = selectedNodeId ? currentProject.agents[selectedNodeId] : null;
  const hasAgents = Object.keys(currentProject.agents).length > 0;
  const hasActionNodes = Object.keys(currentProject.actionNodes || {}).length > 0;
  // Allow running workflows with either agents OR action nodes
  const hasRunnableWorkflow = hasAgents || hasActionNodes;
  const agentTools = selectedNodeId ? currentProject.agents[selectedNodeId]?.tools || [] : [];
  const fnConfig = selectedToolId && currentProject.tool_configs?.[selectedToolId]?.type === 'function' 
    ? currentProject.tool_configs[selectedToolId] as FunctionToolConfig 
    : null;
  
  // Get default prompt from trigger config (for Run button)
  const getDefaultPrompt = (): string => {
    const actionNodes = currentProject.actionNodes || {};
    const trigger = Object.values(actionNodes).find(
      node => node.type === 'trigger' && node.triggerType === 'manual'
    );
    if (trigger && trigger.type === 'trigger' && trigger.manual) {
      return trigger.manual.defaultPrompt || DEFAULT_MANUAL_TRIGGER_CONFIG.defaultPrompt;
    }
    return DEFAULT_MANUAL_TRIGGER_CONFIG.defaultPrompt;
  };

  // Theme-aware colors for ReactFlow components
  const { mode } = useTheme();
  const isLight = mode === 'light';
  const gridColor = isLight ? '#E3E6EA' : '#333';
  const nodeActiveColor = '#4ade80';
  const nodeInactiveColor = isLight ? '#94a3b8' : '#666';

  return (
    <div className="flex flex-col h-full">
      <MenuBar 
        onExportCode={() => setShowCodeEditor(true)} 
        onNewProject={() => setShowNewProjectModal(true)} 
        onTemplateApplied={() => setTimeout(() => applyLayout(), 100)}
        onRunTemplate={() => {
          // Show console and trigger build if needed
          if (!showConsole) toggleConsole();
          if (!builtBinaryPath) {
            handleBuild();
          }
        }}
        buildStatus={buildStatus}
        onBuildStatusClick={() => {
          if (building && isAutobuild) {
            showBuildProgress();
          } else if (buildOutput) {
            // Show the build modal with current output
            // buildOutput is already set, modal will show
          } else if (!builtBinaryPath) {
            handleBuild();
          }
        }}
        debugMode={debugMode}
        onDebugModeToggle={() => setDebugMode(!debugMode)}
      />
      
      <div className="flex flex-1 overflow-hidden">
        {/* Left Sidebar - Palettes */}
        <div 
          className="w-48 border-r p-2 flex flex-col overflow-y-auto"
          style={{ 
            backgroundColor: 'var(--surface-panel)', 
            borderColor: 'var(--border-default)',
            color: 'var(--text-primary)'
          }}
        >
          <AgentPalette onDragStart={onDragStart} onCreate={(type) => {
            createAgentWithUndo(type);
            invalidateBuild('onAgentAdd');
            setTimeout(() => applyLayout(), 100);
          }} />
          <div className="my-2" />
          <ActionPalette onDragStart={onActionDragStart} onCreate={(type) => {
            createActionNode(type);
            // Note: createActionNode already calls applyLayout internally
          }} />
          <div className="my-2" />
          <ToolPalette 
            selectedNodeId={selectedNodeId} 
            agentTools={agentTools} 
            onAdd={handleAddTool} 
            onRemove={t => selectedNodeId && removeToolFromAgent(selectedNodeId, t)} 
          />
          <div className="mt-auto space-y-1.5 pt-2">
            <button 
              onClick={handleCompile} 
              className="w-full px-2 py-1.5 bg-blue-600 hover:bg-blue-700 rounded text-xs text-white font-medium"
            >
              📄 View Code
            </button>
            <div className="flex gap-1">
              <button 
                onClick={() => {
                  if (building && isAutobuild) {
                    // During autobuild, clicking shows the progress modal
                    showBuildProgress();
                  } else {
                    // Normal build
                    handleBuild();
                  }
                }} 
                disabled={building && !isAutobuild} 
                className={`flex-1 px-2 py-1.5 rounded text-xs text-white font-medium ${
                  building 
                    ? 'cursor-pointer' 
                    : builtBinaryPath 
                      ? 'bg-green-600 hover:bg-green-700' 
                      : 'bg-orange-500 hover:bg-orange-600 animate-pulse'
                }`}
                style={building ? { backgroundColor: '#3B82F6' } : undefined}
                title={building && isAutobuild ? 'Click to view build progress' : undefined}
              >
                {building 
                  ? '⏳ Building...'
                  : builtBinaryPath 
                    ? '🔨 Build' 
                    : '🔨 Build Required'}
              </button>
              <button
                onClick={toggleAutobuild}
                className={`px-2 py-1.5 rounded text-xs font-medium transition-colors ${
                  autobuildEnabled 
                    ? 'bg-green-600 hover:bg-green-700 text-white' 
                    : ''
                }`}
                style={!autobuildEnabled ? { 
                  backgroundColor: 'var(--bg-secondary)', 
                  color: 'var(--text-muted)', 
                  border: '1px solid var(--border-default)' 
                } : undefined}
                title={autobuildEnabled ? 'Autobuild ON - builds automatically on changes' : 'Autobuild OFF - click to enable'}
              >
                ⚡
              </button>
            </div>
            <button 
              onClick={toggleConsole} 
              className="w-full px-2 py-1.5 rounded text-xs font-medium"
              style={{ backgroundColor: 'var(--bg-secondary)', color: 'var(--text-primary)', border: '1px solid var(--border-default)' }}
            >
              {showConsole ? 'Hide Console' : 'Show Console'}
            </button>
            {showConsole && debugMode && snapshots.length > 0 && (
              <button 
                onClick={() => setShowStateInspector(!showStateInspector)} 
                className="w-full px-2 py-1.5 rounded text-xs font-medium"
                style={{ backgroundColor: 'var(--bg-secondary)', color: 'var(--text-primary)', border: '1px solid var(--border-default)' }}
              >
                {showStateInspector ? '🔍 Hide Inspector' : '🔍 Show Inspector'}
              </button>
            )}
            <button 
              onClick={() => setShowSettingsModal(true)} 
              className="w-full px-2 py-1.5 rounded text-xs font-medium"
              style={{ backgroundColor: 'var(--bg-secondary)', color: 'var(--text-primary)', border: '1px solid var(--border-default)' }}
            >
              ⚙️ Settings
            </button>
            <button 
              onClick={closeProject} 
              className="w-full px-2 py-1.5 rounded text-xs font-medium"
              style={{ backgroundColor: 'var(--bg-secondary)', color: 'var(--text-primary)', border: '1px solid var(--border-default)' }}
            >
              Back
            </button>
          </div>
        </div>

        {/* Main Canvas Area */}
        <div className="flex-1 relative">
          <ReactFlow 
            nodes={nodes} 
            edges={edges} 
            nodeTypes={nodeTypes} 
            edgeTypes={edgeTypes} 
            onNodesChange={onNodesChange} 
            onEdgesChange={onEdgesChange} 
            onEdgesDelete={onEdgesDelete} 
            onNodesDelete={onNodesDelete} 
            onEdgeDoubleClick={onEdgeDoubleClick} 
            onConnect={onConnect} 
            onNodeClick={onNodeClick} 
            onPaneClick={onPaneClick} 
            onDrop={onDrop} 
            onDragOver={onDragOver} 
            deleteKeyCode={['Backspace', 'Delete']} 
            defaultViewport={{ x: 0, y: 0, zoom: 1 }} 
            minZoom={0.1} 
            maxZoom={2}
            snapToGrid={snapToGrid}
            snapGrid={[gridSize, gridSize]}
          >
            <Background color={gridColor} gap={gridSize} />
            <Controls />
            {showMinimap && (
              <MiniMap 
                nodeColor={n => n.data?.isActive ? nodeActiveColor : nodeInactiveColor} 
                maskColor={isLight ? 'rgba(247, 248, 250, 0.8)' : 'rgba(0, 0, 0, 0.8)'} 
                style={{ background: isLight ? '#F7F8FA' : '#1a1a2e' }} 
              />
            )}
          </ReactFlow>
          <CanvasToolbar 
            onFitView={fitToView}
            showDataFlowOverlay={showDataFlowOverlay}
            onToggleDataFlowOverlay={handleToggleDataFlowOverlay}
            showMinimap={showMinimap}
            onToggleMinimap={toggleMinimap}
            isRunning={flowPhase !== 'idle'}
            isBuilt={!!builtBinaryPath}
            isBuilding={building}
            onRun={() => {
              // Show console if not visible
              if (!showConsole) toggleConsole();
              // Expand console if collapsed
              if (consoleCollapsed) setConsoleCollapsed(false);
              // Set the auto-send prompt to trigger the workflow with the default prompt
              // Use setTimeout to ensure console is rendered first
              setTimeout(() => {
                setAutoSendPrompt(getDefaultPrompt());
              }, 100);
            }}
            onStop={() => {
              // Call the cancel function exposed by TestConsole
              if (cancelFnRef.current) {
                cancelFnRef.current();
              }
            }}
          />
        </div>

        {/* Right Sidebar - Properties Panel */}
        {selectedAgent && selectedNodeId && (
          <PropertiesPanel 
            nodeId={selectedNodeId} 
            agent={selectedAgent} 
            agents={currentProject.agents} 
            toolConfigs={currentProject.tool_configs || {}} 
            onUpdate={updateAgent} 
            onRename={renameAgent} 
            onAddSubAgent={() => addSubAgentToContainer(selectedNodeId)} 
            onClose={() => selectNode(null)} 
            onSelectTool={selectTool} 
            onRemoveTool={t => removeToolFromAgent(selectedNodeId, t)} 
          />
        )}
        
        {/* Right Sidebar - Action Properties Panel (v2.0) */}
        {/* @see Requirements 12.2, 12.3 */}
        {selectedActionNodeId && currentProject && (
          <ActionPropertiesPanel
            nodeId={selectedActionNodeId}
            onClose={() => selectActionNode(null)}
          />
        )}
        
        {selectedToolId && currentProject && (
          <ToolConfigPanel 
            toolId={selectedToolId} 
            config={currentProject.tool_configs?.[selectedToolId] || null} 
            onUpdate={c => updateToolConfig(selectedToolId, c)} 
            onClose={() => selectTool(null)} 
            onOpenCodeEditor={() => setShowCodeEditor(true)} 
          />
        )}
        
        {/* State Inspector Panel - shows runtime state during execution (v2.0) */}
        {/* @see Requirements 4.1, 4.2, 4.5, 5.4 */}
        {/* Only visible when debug mode is enabled */}
        {showConsole && debugMode && showStateInspector && snapshots.length > 0 && (
          <div className="w-72 flex-shrink-0">
            <StateInspector
              snapshot={currentSnapshot}
              previousSnapshot={previousSnapshot}
              snapshots={snapshots}
              currentIndex={currentSnapshotIndex}
              onHistorySelect={handleStateHistorySelect}
              onClose={() => setShowStateInspector(false)}
            />
          </div>
        )}
      </div>

      {/* Timeline View - shows execution history */}
      {/* Only visible when debug mode is enabled */}
      {showConsole && debugMode && showTimeline && hasRunnableWorkflow && builtBinaryPath && snapshots.length > 0 && (
        <TimelineView
          snapshots={snapshots}
          currentIndex={currentSnapshotIndex}
          onScrub={scrubToFn || (() => {})}
          isCollapsed={timelineCollapsed}
          onToggleCollapse={() => setTimelineCollapsed(!timelineCollapsed)}
        />
      )}

      {/* Console Area */}
      {showConsole && hasRunnableWorkflow && builtBinaryPath && (
        <div className={consoleCollapsed ? '' : 'h-64'}>
          <TestConsole 
            onFlowPhase={handleFlowPhase} 
            onActiveAgent={handleActiveAgent} 
            onIteration={setIteration} 
            onThought={handleThought} 
            binaryPath={builtBinaryPath}
            onSnapshotsChange={handleSnapshotsChange}
            buildStatus={buildStatus}
            isCollapsed={consoleCollapsed}
            onCollapseChange={setConsoleCollapsed}
            onInterruptChange={handleInterruptChange}
            autoSendPrompt={autoSendPrompt}
            onAutoSendComplete={() => setAutoSendPrompt(null)}
            onCancelReady={(fn) => { cancelFnRef.current = fn; }}
          />
        </div>
      )}
      {showConsole && hasRunnableWorkflow && !builtBinaryPath && (
        <div 
          className="h-32 border-t flex items-center justify-center"
          style={{ backgroundColor: 'var(--surface-panel)', borderColor: 'var(--border-default)', color: 'var(--text-muted)' }}
        >
          <div className="text-center">
            <div>Build the project first to test it</div>
            <button 
              onClick={handleBuild} 
              className="mt-2 px-4 py-1 bg-blue-600 hover:bg-blue-700 rounded text-white text-sm font-medium"
            >
              Build Project
            </button>
          </div>
        </div>
      )}
      {showConsole && !hasRunnableWorkflow && (
        <div 
          className="h-32 border-t flex items-center justify-center"
          style={{ backgroundColor: 'var(--surface-panel)', borderColor: 'var(--border-default)', color: 'var(--text-muted)' }}
        >
          Drag an Agent or Action node onto the canvas to get started
        </div>
      )}

      {/* Modals */}
      {compiledCode && (
        <CodeModal code={compiledCode} onClose={clearCompiledCode} />
      )}
      {buildOutput && (
        <BuildModal 
          building={building} 
          success={buildOutput.success} 
          output={buildOutput.output} 
          path={buildOutput.path} 
          onClose={clearBuildOutput}
          isAutobuild={isAutobuild}
        />
      )}
      {showCodeEditor && fnConfig && (
        <CodeEditorModal 
          config={fnConfig} 
          onUpdate={c => updateToolConfig(selectedToolId!, c)} 
          onClose={() => setShowCodeEditor(false)} 
        />
      )}
      {showNewProjectModal && (
        <NewProjectModal 
          onConfirm={async (name) => { 
            setShowNewProjectModal(false); 
            const project = await api.projects.create(name); 
            await openProject(project.id); 
            const defaultTemplate = TEMPLATES.find(t => t.id === 'simple_chat'); 
            if (defaultTemplate) { 
              // Add action nodes (including trigger)
              if (defaultTemplate.actionNodes) {
                Object.entries(defaultTemplate.actionNodes).forEach(([id, node]) => {
                  addActionNode(id, node);
                });
              }
              // Add agents
              Object.entries(defaultTemplate.agents).forEach(([id, agent]) => { 
                addAgent(id, agent); 
              }); 
              // Add edges
              defaultTemplate.edges.forEach(e => addProjectEdge(e.from, e.to)); 
              setTimeout(() => applyLayout(), 100); 
            } 
          }} 
          onClose={() => setShowNewProjectModal(false)} 
        />
      )}
      {showSettingsModal && currentProject && (
        <SettingsModal
          settings={currentProject.settings}
          projectName={currentProject.name}
          projectDescription={currentProject.description}
          onSave={(settings, name, description) => {
            updateProjectMeta(name, description);
            updateProjectSettings(settings);
            setShowSettingsModal(false);
          }}
          onClose={() => setShowSettingsModal(false)}
        />
      )}
    </div>
  );
}

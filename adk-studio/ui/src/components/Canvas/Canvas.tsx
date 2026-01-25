import { useCallback, useState, useRef, useMemo, DragEvent } from 'react';
import { ReactFlow, Background, Controls, MiniMap, Node, Edge, Connection } from '@xyflow/react';
import '@xyflow/react/dist/style.css';
import { useStore } from '../../store';
import { TestConsole, BuildStatus } from '../Console/TestConsole';
import { MenuBar } from '../MenuBar';
import { nodeTypes } from '../Nodes';
import { edgeTypes } from '../Edges';
import { AgentPalette, ToolPalette, PropertiesPanel, ToolConfigPanel, StateInspector } from '../Panels';
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
import { TEMPLATES } from '../MenuBar/templates';

type FlowPhase = 'idle' | 'input' | 'output';

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
    // v2.0: Data flow overlay state
    showDataFlowOverlay,
    setShowDataFlowOverlay,
    // Settings
    updateProjectMeta,
    updateProjectSettings,
  } = useStore();

  // Canvas UI state
  const { showConsole, toggleConsole, showMinimap, toggleMinimap } = useCanvasState();
  
  // Build state
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
  } = useBuild(currentProject?.id, currentProject?.settings?.autobuildTriggers);

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
  });
  const { applyLayout, toggleLayout, fitToView, zoomIn, zoomOut } = useLayout();
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
    const edgesBefore = [...(currentProject?.workflow.edges || [])];
    
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
  }, [createAgent, currentProject, undoRedo]);
  
  // Wrapped removeAgent that records for undo
  const removeAgentWithUndo = useCallback((nodeId: string) => {
    if (nodeId === 'START' || nodeId === 'END') return;
    
    // Capture the complete edge state BEFORE removing
    const edgesBefore = [...(currentProject?.workflow.edges || [])];
    
    // Record before removing (with complete edge state)
    undoRedo.recordDeleteNode(nodeId, edgesBefore);
    
    // Then remove
    removeAgent(nodeId);
  }, [removeAgent, undoRedo, currentProject]);
  
  // Clear undo history when project changes
  const prevProjectIdRef = useRef<string | null>(null);
  if (currentProject?.id !== prevProjectIdRef.current) {
    prevProjectIdRef.current = currentProject?.id || null;
    clearUndoHistory();
  }

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
    onDeleteNode: removeAgentWithUndo,
    onDeleteTool: removeToolFromAgent,
    onDuplicateNode: duplicateAgent,
    onSelectNode: selectNode,
    onSelectTool: selectTool,
    onAutoLayout: toggleLayout,
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
    const type = e.dataTransfer.getData('application/reactflow');
    if (type) {
      createAgentWithUndo(type);
      invalidateBuild('onAgentAdd'); // Trigger autobuild when agent is added
      // Apply layout after adding node (only in fixed mode or always for initial setup)
      setTimeout(() => applyLayout(), 100);
    }
  }, [createAgentWithUndo, selectedNodeId, currentProject, addToolToAgent, applyLayout, invalidateBuild]);

  // Connection handlers
  const onConnect = useCallback((p: Connection) => {
    if (p.source && p.target) {
      addProjectEdge(p.source, p.target);
      invalidateBuild('onEdgeAdd'); // Trigger autobuild when edge is added
    }
  }, [addProjectEdge, invalidateBuild]);
  const onEdgesDelete = useCallback((eds: Edge[]) => {
    eds.forEach(e => removeProjectEdge(e.source, e.target));
    if (eds.length > 0) invalidateBuild('onEdgeDelete'); // Trigger autobuild when edges are deleted
  }, [removeProjectEdge, invalidateBuild]);
  const onNodesDelete = useCallback((nds: Node[]) => {
    nds.forEach(n => n.id !== 'START' && n.id !== 'END' && removeAgentWithUndo(n.id));
    if (nds.some(n => n.id !== 'START' && n.id !== 'END')) invalidateBuild('onAgentDelete'); // Trigger autobuild when nodes are deleted
  }, [removeAgentWithUndo, invalidateBuild]);
  const onEdgeDoubleClick = useCallback((_: React.MouseEvent, e: Edge) => {
    removeProjectEdge(e.source, e.target);
    invalidateBuild('onEdgeDelete'); // Trigger autobuild when edge is deleted
  }, [removeProjectEdge, invalidateBuild]);
  const onNodeClick = useCallback((_: React.MouseEvent, n: Node) => selectNode(n.id !== 'START' && n.id !== 'END' ? n.id : null), [selectNode]);
  const onPaneClick = useCallback(() => selectNode(null), [selectNode]);

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
  const agentTools = selectedNodeId ? currentProject.agents[selectedNodeId]?.tools || [] : [];
  const fnConfig = selectedToolId && currentProject.tool_configs?.[selectedToolId]?.type === 'function' 
    ? currentProject.tool_configs[selectedToolId] as FunctionToolConfig 
    : null;

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
          <AgentPalette onDragStart={onDragStart} onCreate={createAgent} />
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
                onClick={handleBuild} 
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
                  ? (isAutobuild ? '⚡ Auto Building...' : '⏳ Building...') 
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
            {showConsole && snapshots.length > 0 && (
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
            onAutoLayout={toggleLayout} 
            onFitView={fitToView}
            showDataFlowOverlay={showDataFlowOverlay}
            onToggleDataFlowOverlay={handleToggleDataFlowOverlay}
            showMinimap={showMinimap}
            onToggleMinimap={toggleMinimap}
            isRunning={flowPhase !== 'idle'}
            onRun={() => {
              // Show console and trigger build if needed
              if (!showConsole) toggleConsole();
              // Focus on the chat input to prompt user to send a message
            }}
            onStop={() => {
              // Stop is handled by TestConsole's cancel function
              // This is a visual indicator - actual stop is in console
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
        {showConsole && showStateInspector && snapshots.length > 0 && (
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
      {showConsole && hasAgents && builtBinaryPath && snapshots.length > 0 && (
        <TimelineView
          snapshots={snapshots}
          currentIndex={currentSnapshotIndex}
          onScrub={scrubToFn || (() => {})}
          isCollapsed={timelineCollapsed}
          onToggleCollapse={() => setTimelineCollapsed(!timelineCollapsed)}
        />
      )}

      {/* Console Area */}
      {showConsole && hasAgents && builtBinaryPath && (
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
          />
        </div>
      )}
      {showConsole && hasAgents && !builtBinaryPath && (
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
      {showConsole && !hasAgents && (
        <div 
          className="h-32 border-t flex items-center justify-center"
          style={{ backgroundColor: 'var(--surface-panel)', borderColor: 'var(--border-default)', color: 'var(--text-muted)' }}
        >
          Drag "LLM Agent" onto the canvas to get started
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
              Object.entries(defaultTemplate.agents).forEach(([id, agent]) => { 
                addAgent(id, agent); 
              }); 
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

import { useCallback, useState, useRef, useMemo, DragEvent } from 'react';
import { ReactFlow, Background, Controls, MiniMap, Node, Edge, Connection } from '@xyflow/react';
import '@xyflow/react/dist/style.css';
import { useStore } from '../../store';
import { TestConsole, BuildStatus } from '../Console/TestConsole';
import { MenuBar } from '../MenuBar';
import { nodeTypes } from '../Nodes';
import { edgeTypes } from '../Edges';
import { AgentPalette, ToolPalette, PropertiesPanel, ToolConfigPanel, StateInspector } from '../Panels';
import { CodeModal, BuildModal, CodeEditorModal, NewProjectModal } from '../Overlays';
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
import type { FunctionToolConfig, AgentSchema, ToolConfig } from '../../types/project';
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
  } = useStore();

  // Canvas UI state
  const { showConsole, toggleConsole, showMinimap } = useCanvasState();
  
  // Build state
  const { 
    building, 
    buildOutput, 
    builtBinaryPath, 
    compiledCode, 
    build: handleBuild, 
    compile: handleCompile, 
    clearBuildOutput, 
    clearCompiledCode,
    invalidateBuild,
  } = useBuild(currentProject?.id);

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
  const { applyLayout, toggleLayout, fitToView } = useLayout();
  const { createAgent, duplicateAgent, removeAgent } = useAgentActions();

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
    invalidateBuild();
    debouncedSave();
  }, [storeUpdateAgent, invalidateBuild, debouncedSave]);

  // Tool config update with save and build invalidation
  const updateToolConfig = useCallback((toolId: string, config: ToolConfig) => {
    storeUpdateToolConfig(toolId, config);
    invalidateBuild();
    debouncedSave();
  }, [storeUpdateToolConfig, invalidateBuild, debouncedSave]);

  // Keyboard shortcuts
  useKeyboardShortcuts({
    selectedNodeId, 
    selectedToolId,
    onDeleteNode: removeAgent,
    onDeleteTool: removeToolFromAgent,
    onDuplicateNode: duplicateAgent,
    onSelectNode: selectNode,
    onSelectTool: selectTool,
    onAutoLayout: toggleLayout,
    onFitView: fitToView,
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
      return;
    }
    const type = e.dataTransfer.getData('application/reactflow');
    if (type) {
      createAgent(type);
      // Apply layout after adding node (only in fixed mode or always for initial setup)
      setTimeout(() => applyLayout(), 100);
    }
  }, [createAgent, selectedNodeId, currentProject, addToolToAgent, applyLayout]);

  // Connection handlers
  const onConnect = useCallback((p: Connection) => p.source && p.target && addProjectEdge(p.source, p.target), [addProjectEdge]);
  const onEdgesDelete = useCallback((eds: Edge[]) => eds.forEach(e => removeProjectEdge(e.source, e.target)), [removeProjectEdge]);
  const onNodesDelete = useCallback((nds: Node[]) => nds.forEach(n => n.id !== 'START' && n.id !== 'END' && removeAgent(n.id)), [removeAgent]);
  const onEdgeDoubleClick = useCallback((_: React.MouseEvent, e: Edge) => removeProjectEdge(e.source, e.target), [removeProjectEdge]);
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
            <button 
              onClick={handleBuild} 
              disabled={building} 
              className={`w-full px-2 py-1.5 rounded text-xs text-white font-medium ${
                building 
                  ? 'opacity-50 cursor-not-allowed' 
                  : builtBinaryPath 
                    ? 'bg-green-600 hover:bg-green-700' 
                    : 'bg-orange-500 hover:bg-orange-600 animate-pulse'
              }`}
              style={building ? { backgroundColor: 'var(--text-muted)' } : undefined}
            >
              {building ? '⏳ Building...' : builtBinaryPath ? '🔨 Build' : '🔨 Build Required'}
            </button>
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
    </div>
  );
}

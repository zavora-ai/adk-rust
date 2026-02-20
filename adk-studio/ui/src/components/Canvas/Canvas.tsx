import { useCallback, useState, useRef, useEffect } from 'react';
import { ReactFlow, Background, Controls, MiniMap } from '@xyflow/react';
import '@xyflow/react/dist/style.css';
import { useStore } from '../../store';
import type { BuildStatus } from '../Console/TestConsole';
import { MenuBar } from '../MenuBar';
import { nodeTypes } from '../Nodes';
import { edgeTypes } from '../Edges';
import { CanvasModals } from './CanvasModals';
import { CanvasBottomPanel } from './CanvasBottomPanel';
import { CanvasSidebar } from './CanvasSidebar';
import { CanvasRightPanel } from './CanvasRightPanel';
import { CanvasToolbar } from './CanvasToolbar';
import { api } from '../../api/client';
import { useKeyboardShortcuts } from '../../hooks/useKeyboardShortcuts';
import { useLayout } from '../../hooks/useLayout';
import { useCanvasNodes } from '../../hooks/useCanvasNodes';
import { useAgentActions } from '../../hooks/useAgentActions';
import { useCanvasState } from '../../hooks/useCanvasState';
import { useBuild } from '../../hooks/useBuild';
import { useTheme } from '../../hooks/useTheme';
import { useCanvasDragDrop } from '../../hooks/useCanvasDragDrop';
import { useCanvasExecution } from '../../hooks/useCanvasExecution';
import { useCanvasConnections } from '../../hooks/useCanvasConnections';
import { useCanvasUndoRedo } from '../../hooks/useCanvasUndoRedo';
import type { FunctionToolConfig } from '../../types/project';
import { DEFAULT_MANUAL_TRIGGER_CONFIG } from '../../types/actionNodes';
import { TEMPLATES } from '../MenuBar/templates';

export function Canvas() {
  const {
    currentProject, openProject, closeProject, saveProject,
    selectedNodeId, selectedToolId, selectTool, renameAgent,
    addEdge: addProjectEdge, removeToolFromAgent, addSubAgentToContainer,
    addAgent, addActionNode, selectedActionNodeId, selectActionNode, selectNode,
    showDataFlowOverlay, setShowDataFlowOverlay, debugMode, setDebugMode,
    updateProjectMeta, updateProjectSettings, snapToGrid, gridSize,
    showApiKeySetup, clearApiKeySetup,
  } = useStore();

  const handleUISettingChange = useCallback((key: string, value: boolean) => {
    updateProjectSettings({ [key]: value });
  }, [updateProjectSettings]);
  const { showConsole, toggleConsole, showMinimap, toggleMinimap, showTimeline } = useCanvasState(currentProject?.settings, handleUISettingChange);

  const canBuild = useCallback(() => {
    if (!currentProject) return false;
    const ac = Object.keys(currentProject.agents).length;
    const anc = Object.keys(currentProject.actionNodes || {}).length;
    return (ac > 0 || anc > 0) && currentProject.workflow.edges.length > 0;
  }, [currentProject]);

  const {
    building, buildOutput, builtBinaryPath, compiledCode, autobuildEnabled, isAutobuild,
    build: handleBuild, compile: handleCompile, clearBuildOutput, clearCompiledCode,
    invalidateBuild, toggleAutobuild, showBuildProgress, setBinaryPath,
  } = useBuild(currentProject?.id, currentProject?.settings?.autobuildTriggers, currentProject?.settings?.autobuildEnabled, canBuild);
  const buildStatus: BuildStatus = building ? 'building'
    : buildOutput?.success ? 'success'
    : buildOutput && !buildOutput.success ? 'error'
    : builtBinaryPath ? 'success' : 'none';

  const [consoleCollapsed, setConsoleCollapsed] = useState(false);
  const [autoSendPrompt, setAutoSendPrompt] = useState<string | null>(null);
  const cancelFnRef = useRef<(() => void) | null>(null);
  const [showCodeEditor, setShowCodeEditor] = useState(false);
  const [showNewProjectModal, setShowNewProjectModal] = useState(false);
  const [showSettingsModal, setShowSettingsModal] = useState(false);
  const [settingsInitialTab, setSettingsInitialTab] = useState<'general' | 'codegen' | 'ui' | 'env' | undefined>(undefined);
  const [showApiKeyBanner, setShowApiKeyBanner] = useState(false);

  // Auto-open Settings modal on Environment tab when API key setup is needed (new project flow)
  useEffect(() => {
    if (showApiKeySetup) {
      setSettingsInitialTab('env');
      setShowApiKeyBanner(true);
      setShowSettingsModal(true);
      clearApiKeySetup();
    }
  }, [showApiKeySetup, clearApiKeySetup]);

  const execution = useCanvasExecution({ showDataFlowOverlay, setShowDataFlowOverlay });
  const { applyLayout, fitToView, zoomIn, zoomOut } = useLayout();
  const { createAgent, duplicateAgent, removeAgent } = useAgentActions();

  const nodeCount = currentProject
    ? Object.keys(currentProject.agents).length + Object.keys(currentProject.actionNodes || {}).length : 0;
  const { undoRedo, createAgentWithUndo, removeAgentWithUndo, removeActionNodeWithLayout } = useCanvasUndoRedo({
    createAgent,
    removeAgent,
    removeActionNode: useStore.getState().removeActionNode,
    applyLayout,
    invalidateBuild,
    currentProjectId: currentProject?.id,
    nodeCount,
  });

  const saveTimerRef = useRef<ReturnType<typeof setTimeout> | null>(null);
  const debouncedSave = useCallback(() => {
    if (saveTimerRef.current) clearTimeout(saveTimerRef.current);
    saveTimerRef.current = setTimeout(() => saveProject(), 500);
  }, [saveProject]);

  const {
    onConnect, onEdgesDelete, onNodesDelete, onEdgeDoubleClick, onNodeClick, onPaneClick,
    updateAgent, updateToolConfig, handleAddTool,
  } = useCanvasConnections({ currentProject, removeAgentWithUndo, removeActionNode: removeActionNodeWithLayout, invalidateBuild, applyLayout, debouncedSave });

  const { nodes, edges, onNodesChange, onEdgesChange } = useCanvasNodes(currentProject, {
    activeAgent: execution.activeAgent,
    iteration: execution.iteration,
    flowPhase: execution.flowPhase,
    thoughts: execution.thoughts,
    stateKeys: execution.stateKeys,
    showDataFlowOverlay,
    highlightedKey: execution.highlightedKey,
    onKeyHover: execution.handleKeyHover,
    executionPath: execution.executionPath.path,
    isExecuting: execution.executionPath.isExecuting,
    interruptedNodeId: execution.interruptedNodeId,
  });

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
    onUndo: undoRedo.undo,
    onRedo: undoRedo.redo,
    onRun: builtBinaryPath ? () => {
      if (!showConsole) toggleConsole();
      if (consoleCollapsed) setConsoleCollapsed(false);
      setTimeout(() => setAutoSendPrompt(getDefaultPrompt()), 100);
    } : undefined,
  });

  const { onDragStart, onActionDragStart, onDragOver, onDrop, createActionNode } = useCanvasDragDrop({
    createAgentWithUndo,
    selectedNodeId,
    applyLayout,
    invalidateBuild,
  });

  if (!currentProject) return null;

  // Derived state
  const selectedAgent = selectedNodeId ? currentProject.agents[selectedNodeId] : null;
  const hasRunnableWorkflow = Object.keys(currentProject.agents).length > 0 || Object.keys(currentProject.actionNodes || {}).length > 0;
  const agentTools = selectedNodeId ? currentProject.agents[selectedNodeId]?.tools || [] : [];
  const fnConfig = selectedToolId && currentProject.tool_configs?.[selectedToolId]?.type === 'function'
    ? currentProject.tool_configs[selectedToolId] as FunctionToolConfig
    : null;

  // Get default prompt from trigger config
  const getDefaultPrompt = (): string => {
    const actionNodes = currentProject.actionNodes || {};
    const trigger = Object.values(actionNodes).find(n => n.type === 'trigger' && n.triggerType === 'manual');
    if (trigger && trigger.type === 'trigger' && trigger.manual) {
      return trigger.manual.defaultPrompt || DEFAULT_MANUAL_TRIGGER_CONFIG.defaultPrompt;
    }
    return DEFAULT_MANUAL_TRIGGER_CONFIG.defaultPrompt;
  };

  // New project creation handler
  const handleNewProjectConfirm = useCallback(async (name: string) => {
    setShowNewProjectModal(false);
    const project = await api.projects.create(name);
    await openProject(project.id);
    const defaultTemplate = TEMPLATES.find(t => t.id === 'simple_chat');
    if (defaultTemplate) {
      if (defaultTemplate.actionNodes) {
        Object.entries(defaultTemplate.actionNodes).forEach(([id, node]) => { addActionNode(id, node); });
      }
      Object.entries(defaultTemplate.agents).forEach(([id, agent]) => { addAgent(id, agent); });
      defaultTemplate.edges.forEach(e => addProjectEdge(e.from, e.to));
      setTimeout(() => applyLayout(), 100);
    }
  }, [openProject, addActionNode, addAgent, addProjectEdge, applyLayout]);

  // Settings save handler
  const handleSaveSettings = useCallback((settings: import('../../types/project').ProjectSettings, name: string, description: string) => {
    updateProjectMeta(name, description);
    updateProjectSettings(settings);
    setShowSettingsModal(false);
    setSettingsInitialTab(undefined);
    setShowApiKeyBanner(false);
  }, [updateProjectMeta, updateProjectSettings]);

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
          if (!showConsole) toggleConsole();
          if (!builtBinaryPath) handleBuild();
        }}
        buildStatus={buildStatus}
        onBuildStatusClick={() => {
          if (building && isAutobuild) showBuildProgress();
          else if (buildOutput) { /* buildOutput is already set, modal will show */ }
          else if (!builtBinaryPath) handleBuild();
        }}
        debugMode={debugMode}
        onDebugModeToggle={() => setDebugMode(!debugMode)}
      />

      <div className="flex flex-1 overflow-hidden">
        {/* Left Sidebar - Palettes */}
        <CanvasSidebar
          onAgentDragStart={onDragStart}
          onAgentCreate={(type) => { createAgentWithUndo(type); invalidateBuild('onAgentAdd'); setTimeout(() => applyLayout(), 100); }}
          onActionDragStart={onActionDragStart}
          onActionCreate={(type) => createActionNode(type)}
          selectedNodeId={selectedNodeId}
          agentTools={agentTools}
          onToolAdd={handleAddTool}
          onToolRemove={t => selectedNodeId && removeToolFromAgent(selectedNodeId, t)}
          onCompile={handleCompile}
          onBuild={handleBuild}
          building={building}
          isAutobuild={isAutobuild}
          builtBinaryPath={builtBinaryPath}
          autobuildEnabled={autobuildEnabled}
          onToggleAutobuild={toggleAutobuild}
          onShowBuildProgress={showBuildProgress}
          showConsole={showConsole}
          onToggleConsole={toggleConsole}
          debugMode={debugMode}
          snapshotCount={execution.snapshots.length}
          showStateInspector={execution.showStateInspector}
          onToggleStateInspector={() => execution.setShowStateInspector(!execution.showStateInspector)}
          onShowSettings={() => setShowSettingsModal(true)}
          onCloseProject={closeProject}
        />

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
            proOptions={{ hideAttribution: true }}
            connectionLineStyle={{ stroke: 'var(--accent-primary)', strokeWidth: 2 }}
            defaultEdgeOptions={{ type: 'animated' }}
            fitView
            fitViewOptions={{ padding: 0.2, maxZoom: 0.9 }}
            nodesDraggable
            nodesConnectable
            elementsSelectable
            panOnDrag
            zoomOnScroll
            zoomOnPinch
            selectNodesOnDrag={false}
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
            onToggleDataFlowOverlay={execution.handleToggleDataFlowOverlay}
            showMinimap={showMinimap}
            onToggleMinimap={toggleMinimap}
            isRunning={execution.flowPhase !== 'idle'}
            isBuilt={!!builtBinaryPath}
            isBuilding={building}
            onRun={() => {
              if (!showConsole) toggleConsole();
              if (consoleCollapsed) setConsoleCollapsed(false);
              setTimeout(() => setAutoSendPrompt(getDefaultPrompt()), 100);
            }}
            onStop={() => { if (cancelFnRef.current) cancelFnRef.current(); }}
          />
        </div>

        {/* Right Sidebar - Properties, Action Properties, Tool Config, State Inspector */}
        <CanvasRightPanel
          selectedNodeId={selectedNodeId}
          selectedAgent={selectedAgent}
          agents={currentProject.agents}
          toolConfigs={currentProject.tool_configs || {}}
          onUpdateAgent={updateAgent}
          onRenameAgent={renameAgent}
          onAddSubAgent={(nodeId) => addSubAgentToContainer(nodeId)}
          onCloseAgent={() => selectNode(null)}
          onSelectTool={selectTool}
          onRemoveToolFromAgent={(nodeId, t) => removeToolFromAgent(nodeId, t)}
          selectedActionNodeId={selectedActionNodeId}
          hasProject={!!currentProject}
          onCloseActionNode={() => selectActionNode(null)}
          selectedToolId={selectedToolId}
          toolConfig={currentProject.tool_configs?.[selectedToolId || ''] || null}
          onUpdateToolConfig={updateToolConfig}
          onCloseTool={() => selectTool(null)}
          onOpenCodeEditor={() => setShowCodeEditor(true)}
          showConsole={showConsole}
          debugMode={debugMode}
          showStateInspector={execution.showStateInspector}
          snapshots={execution.snapshots}
          currentSnapshot={execution.currentSnapshot}
          previousSnapshot={execution.previousSnapshot}
          currentSnapshotIndex={execution.currentSnapshotIndex}
          onStateHistorySelect={execution.handleStateHistorySelect}
          onCloseStateInspector={() => execution.setShowStateInspector(false)}
        />
      </div>

      {/* Bottom Panel - Timeline, Console, Placeholders */}
      <CanvasBottomPanel
        showConsole={showConsole}
        showTimeline={showTimeline}
        debugMode={debugMode}
        hasRunnableWorkflow={hasRunnableWorkflow}
        builtBinaryPath={builtBinaryPath}
        snapshots={execution.snapshots}
        currentSnapshotIndex={execution.currentSnapshotIndex}
        scrubToFn={execution.scrubToFn}
        timelineCollapsed={execution.timelineCollapsed}
        onToggleTimelineCollapse={() => execution.setTimelineCollapsed(!execution.timelineCollapsed)}
        consoleCollapsed={consoleCollapsed}
        onConsoleCollapseChange={setConsoleCollapsed}
        buildStatus={buildStatus}
        autoSendPrompt={autoSendPrompt}
        onAutoSendComplete={() => setAutoSendPrompt(null)}
        onCancelReady={(fn) => { cancelFnRef.current = fn; }}
        onFlowPhase={execution.handleFlowPhase}
        onActiveAgent={execution.handleActiveAgent}
        onIteration={execution.setIteration}
        onThought={execution.handleThought}
        onSnapshotsChange={execution.handleSnapshotsChange}
        onInterruptChange={execution.handleInterruptChange}
        onBuild={handleBuild}
        onBinaryPathDetected={setBinaryPath}
      />

      {/* Modals */}
      <CanvasModals
        compiledCode={compiledCode}
        onClearCompiledCode={clearCompiledCode}
        buildOutput={buildOutput}
        building={building}
        isAutobuild={isAutobuild}
        onClearBuildOutput={clearBuildOutput}
        showCodeEditor={showCodeEditor}
        fnConfig={fnConfig}
        selectedToolId={selectedToolId}
        onUpdateToolConfig={updateToolConfig}
        onCloseCodeEditor={() => setShowCodeEditor(false)}
        showNewProjectModal={showNewProjectModal}
        onNewProjectConfirm={handleNewProjectConfirm}
        onCloseNewProjectModal={() => setShowNewProjectModal(false)}
        showSettingsModal={showSettingsModal}
        projectId={currentProject.id}
        projectSettings={currentProject.settings}
        projectName={currentProject.name}
        projectDescription={currentProject.description}
        settingsInitialTab={settingsInitialTab}
        showApiKeyBanner={showApiKeyBanner}
        onSaveSettings={handleSaveSettings}
        onCloseSettingsModal={() => {
          setShowSettingsModal(false);
          setSettingsInitialTab(undefined);
          setShowApiKeyBanner(false);
        }}
      />
    </div>
  );
}

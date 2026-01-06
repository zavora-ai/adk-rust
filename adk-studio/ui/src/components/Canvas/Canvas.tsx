import { useCallback, useState, useRef, DragEvent } from 'react';
import { ReactFlow, Background, Controls, MiniMap, Node, Edge, Connection } from '@xyflow/react';
import '@xyflow/react/dist/style.css';
import { useStore } from '../../store';
import { TestConsole } from '../Console/TestConsole';
import { MenuBar } from '../MenuBar';
import { nodeTypes } from '../Nodes';
import { edgeTypes } from '../Edges';
import { AgentPalette, ToolPalette, PropertiesPanel, ToolConfigPanel } from '../Panels';
import { CodeModal, BuildModal, CodeEditorModal, NewProjectModal } from '../Overlays';
import { CanvasToolbar } from './CanvasToolbar';
import { api, GeneratedProject } from '../../api/client';
import { useKeyboardShortcuts } from '../../hooks/useKeyboardShortcuts';
import { useLayout } from '../../hooks/useLayout';
import { useCanvasNodes } from '../../hooks/useCanvasNodes';
import { useAgentActions } from '../../hooks/useAgentActions';
import type { FunctionToolConfig, AgentSchema, ToolConfig } from '../../types/project';
import { TEMPLATES } from '../MenuBar/templates';

type FlowPhase = 'idle' | 'input' | 'output';

export function Canvas() {
  const { currentProject, openProject, closeProject, saveProject, selectNode, selectedNodeId, updateAgent: storeUpdateAgent, renameAgent, addEdge: addProjectEdge, removeEdge: removeProjectEdge, addToolToAgent, removeToolFromAgent, addSubAgentToContainer, selectedToolId, selectTool, updateToolConfig: storeUpdateToolConfig, addAgent } = useStore();
  const [showConsole, setShowConsole] = useState(true);
  const [flowPhase, setFlowPhase] = useState<FlowPhase>('idle');
  const [activeAgent, setActiveAgent] = useState<string | null>(null);
  const [iteration, setIteration] = useState(0);
  const [thoughts, setThoughts] = useState<Record<string, string>>({});
  const [compiledCode, setCompiledCode] = useState<GeneratedProject | null>(null);
  const [buildOutput, setBuildOutput] = useState<{ success: boolean, output: string, path: string | null } | null>(null);
  const [building, setBuilding] = useState(false);
  const [builtBinaryPath, setBuiltBinaryPath] = useState<string | null>(null);
  const [showCodeEditor, setShowCodeEditor] = useState(false);
  const [showNewProjectModal, setShowNewProjectModal] = useState(false);

  const { nodes, edges, onNodesChange, onEdgesChange } = useCanvasNodes(currentProject, { activeAgent, iteration, flowPhase, thoughts });
  const { applyLayout, toggleLayout, fitToView } = useLayout();
  const { createAgent, duplicateAgent, removeAgent } = useAgentActions();

  const handleThought = useCallback((agent: string, thought: string | null) => {
    setThoughts(prev => thought ? { ...prev, [agent]: thought } : Object.fromEntries(Object.entries(prev).filter(([k]) => k !== agent)));
  }, []);

  const saveTimerRef = useRef<ReturnType<typeof setTimeout> | null>(null);
  const debouncedSave = useCallback(() => {
    if (saveTimerRef.current) clearTimeout(saveTimerRef.current);
    saveTimerRef.current = setTimeout(() => saveProject(), 500);
  }, [saveProject]);

  const updateAgent = useCallback((id: string, updates: Partial<AgentSchema>) => {
    storeUpdateAgent(id, updates);
    setBuiltBinaryPath(null);
    debouncedSave();
  }, [storeUpdateAgent, debouncedSave]);

  const updateToolConfig = useCallback((toolId: string, config: ToolConfig) => {
    storeUpdateToolConfig(toolId, config);
    setBuiltBinaryPath(null);
    debouncedSave();
  }, [storeUpdateToolConfig, debouncedSave]);

  useKeyboardShortcuts({
    selectedNodeId, selectedToolId,
    onDeleteNode: removeAgent,
    onDeleteTool: removeToolFromAgent,
    onDuplicateNode: duplicateAgent,
    onSelectNode: selectNode,
    onSelectTool: selectTool,
    onAutoLayout: toggleLayout,
    onFitView: fitToView,
  });

  const handleCompile = useCallback(async () => {
    if (!currentProject) return;
    try { setCompiledCode(await api.projects.compile(currentProject.id)); }
    catch (e) { alert('Compile failed: ' + (e as Error).message); }
  }, [currentProject]);

  const handleBuild = useCallback(async () => {
    if (!currentProject) return;
    setBuilding(true);
    setBuildOutput({ success: false, output: '', path: null });
    const es = new EventSource(`/api/projects/${currentProject.id}/build-stream`);
    let output = '';
    es.addEventListener('status', (e) => { output += e.data + '\n'; setBuildOutput({ success: false, output, path: null }); });
    es.addEventListener('output', (e) => { output += e.data + '\n'; setBuildOutput({ success: false, output, path: null }); });
    es.addEventListener('done', (e) => { setBuildOutput({ success: true, output, path: e.data }); setBuiltBinaryPath(e.data); setBuilding(false); es.close(); });
    es.addEventListener('error', (e) => { output += '\nError: ' + ((e as MessageEvent).data || 'Build failed'); setBuildOutput({ success: false, output, path: null }); setBuilding(false); es.close(); });
    es.onerror = () => { setBuilding(false); es.close(); };
  }, [currentProject]);

  const onDragStart = (e: DragEvent, type: string) => { e.dataTransfer.setData('application/reactflow', type); e.dataTransfer.effectAllowed = 'move'; };
  const onDragOver = useCallback((e: DragEvent) => { e.preventDefault(); e.dataTransfer.dropEffect = e.dataTransfer.types.includes('text/plain') ? 'copy' : 'move'; }, []);
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
      setTimeout(() => applyLayout(), 100);
    }
  }, [createAgent, selectedNodeId, currentProject, addToolToAgent, applyLayout]);

  const onConnect = useCallback((p: Connection) => p.source && p.target && addProjectEdge(p.source, p.target), [addProjectEdge]);
  const onEdgesDelete = useCallback((eds: Edge[]) => eds.forEach(e => removeProjectEdge(e.source, e.target)), [removeProjectEdge]);
  const onNodesDelete = useCallback((nds: Node[]) => nds.forEach(n => n.id !== 'START' && n.id !== 'END' && removeAgent(n.id)), [removeAgent]);
  const onEdgeDoubleClick = useCallback((_: React.MouseEvent, e: Edge) => removeProjectEdge(e.source, e.target), [removeProjectEdge]);
  const onNodeClick = useCallback((_: React.MouseEvent, n: Node) => selectNode(n.id !== 'START' && n.id !== 'END' ? n.id : null), [selectNode]);
  const onPaneClick = useCallback(() => selectNode(null), [selectNode]);

  const handleAddTool = useCallback((type: string) => {
    if (!selectedNodeId) return;
    addToolToAgent(selectedNodeId, type);
    const tools = currentProject?.agents[selectedNodeId]?.tools || [];
    const isMulti = type === 'function' || type === 'mcp';
    const newId = isMulti ? `${selectedNodeId}_${type}_${tools.filter(t => t.startsWith(type)).length + 1}` : `${selectedNodeId}_${type}`;
    setTimeout(() => selectTool(newId), 0);
  }, [selectedNodeId, currentProject, addToolToAgent, selectTool]);

  if (!currentProject) return null;
  const selectedAgent = selectedNodeId ? currentProject.agents[selectedNodeId] : null;
  const hasAgents = Object.keys(currentProject.agents).length > 0;
  const agentTools = selectedNodeId ? currentProject.agents[selectedNodeId]?.tools || [] : [];
  const fnConfig = selectedToolId && currentProject.tool_configs?.[selectedToolId]?.type === 'function' ? currentProject.tool_configs[selectedToolId] as FunctionToolConfig : null;

  return (
    <div className="flex flex-col h-full">
      <MenuBar onExportCode={() => setShowCodeEditor(true)} onNewProject={() => setShowNewProjectModal(true)} onTemplateApplied={() => setTimeout(() => applyLayout(), 100)} />
      <div className="flex flex-1 overflow-hidden">
        <div className="w-48 bg-studio-panel border-r border-gray-700 p-2 flex flex-col overflow-y-auto">
          <AgentPalette onDragStart={onDragStart} onCreate={createAgent} />
          <div className="my-2" />
          <ToolPalette selectedNodeId={selectedNodeId} agentTools={agentTools} onAdd={handleAddTool} onRemove={t => selectedNodeId && removeToolFromAgent(selectedNodeId, t)} />
          <div className="mt-auto space-y-1.5 pt-2">
            <button onClick={handleCompile} className="w-full px-2 py-1.5 bg-blue-700 hover:bg-blue-600 rounded text-xs">📄 View Code</button>
            <button onClick={handleBuild} disabled={building} className={`w-full px-2 py-1.5 rounded text-xs ${building ? 'bg-gray-600' : builtBinaryPath ? 'bg-green-700 hover:bg-green-600' : 'bg-orange-600 hover:bg-orange-500 animate-pulse'}`}>
              {building ? '⏳ Building...' : builtBinaryPath ? '🔨 Build' : '🔨 Build Required'}
            </button>
            <button onClick={() => setShowConsole(!showConsole)} className="w-full px-2 py-1.5 bg-gray-700 rounded text-xs">{showConsole ? 'Hide Console' : 'Show Console'}</button>
            <button onClick={closeProject} className="w-full px-2 py-1.5 bg-gray-700 rounded text-xs">Back</button>
          </div>
        </div>

        <div className="flex-1 relative">
          <ReactFlow nodes={nodes} edges={edges} nodeTypes={nodeTypes} edgeTypes={edgeTypes} onNodesChange={onNodesChange} onEdgesChange={onEdgesChange} onEdgesDelete={onEdgesDelete} onNodesDelete={onNodesDelete} onEdgeDoubleClick={onEdgeDoubleClick} onConnect={onConnect} onNodeClick={onNodeClick} onPaneClick={onPaneClick} onDrop={onDrop} onDragOver={onDragOver} deleteKeyCode={['Backspace', 'Delete']} defaultViewport={{ x: 0, y: 0, zoom: 1 }} minZoom={0.1} maxZoom={2}>
            <Background color="#333" gap={20} />
            <Controls />
            <MiniMap nodeColor={n => n.data?.isActive ? '#4ade80' : '#666'} maskColor="rgba(0,0,0,0.8)" style={{ background: '#1a1a2e' }} />
          </ReactFlow>
          <CanvasToolbar onAutoLayout={toggleLayout} onFitView={fitToView} />
        </div>

        {selectedAgent && selectedNodeId && (
          <PropertiesPanel nodeId={selectedNodeId} agent={selectedAgent} agents={currentProject.agents} toolConfigs={currentProject.tool_configs || {}} onUpdate={updateAgent} onRename={renameAgent} onAddSubAgent={() => addSubAgentToContainer(selectedNodeId)} onClose={() => selectNode(null)} onSelectTool={selectTool} onRemoveTool={t => removeToolFromAgent(selectedNodeId, t)} />
        )}
        {selectedToolId && currentProject && (
          <ToolConfigPanel toolId={selectedToolId} config={currentProject.tool_configs?.[selectedToolId] || null} onUpdate={c => updateToolConfig(selectedToolId, c)} onClose={() => selectTool(null)} onOpenCodeEditor={() => setShowCodeEditor(true)} />
        )}
      </div>

      {showConsole && hasAgents && builtBinaryPath && <div className="h-64"><TestConsole onFlowPhase={setFlowPhase} onActiveAgent={setActiveAgent} onIteration={setIteration} onThought={handleThought} binaryPath={builtBinaryPath} /></div>}
      {showConsole && hasAgents && !builtBinaryPath && (
        <div className="h-32 bg-studio-panel border-t border-gray-700 flex items-center justify-center text-gray-500">
          <div className="text-center"><div>Build the project first to test it</div><button onClick={handleBuild} className="mt-2 px-4 py-1 bg-blue-600 hover:bg-blue-700 rounded text-white text-sm">Build Project</button></div>
        </div>
      )}
      {showConsole && !hasAgents && <div className="h-32 bg-studio-panel border-t border-gray-700 flex items-center justify-center text-gray-500">Drag "LLM Agent" onto the canvas to get started</div>}

      {compiledCode && <CodeModal code={compiledCode} onClose={() => setCompiledCode(null)} />}
      {buildOutput && <BuildModal building={building} success={buildOutput.success} output={buildOutput.output} path={buildOutput.path} onClose={() => setBuildOutput(null)} />}
      {showCodeEditor && fnConfig && <CodeEditorModal config={fnConfig} onUpdate={c => updateToolConfig(selectedToolId!, c)} onClose={() => setShowCodeEditor(false)} />}
      {showNewProjectModal && <NewProjectModal onConfirm={async (name) => { setShowNewProjectModal(false); const project = await api.projects.create(name); await openProject(project.id); const defaultTemplate = TEMPLATES.find(t => t.id === 'simple_chat'); if (defaultTemplate) { Object.entries(defaultTemplate.agents).forEach(([id, agent]) => { addAgent(id, agent); }); defaultTemplate.edges.forEach(e => addProjectEdge(e.from, e.to)); setTimeout(() => applyLayout(), 100); } }} onClose={() => setShowNewProjectModal(false)} />}
    </div>
  );
}

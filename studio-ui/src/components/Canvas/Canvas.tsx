import { useCallback, useEffect, useState, useRef, DragEvent } from 'react';
import { ReactFlow, Background, Controls, Node, Edge, useNodesState, useEdgesState, Connection } from '@xyflow/react';
import '@xyflow/react/dist/style.css';
import { useStore } from '../../store';
import { TestConsole } from '../Console/TestConsole';
import { MenuBar } from '../MenuBar';
import { nodeTypes } from '../Nodes';
import { AgentPalette, ToolPalette, PropertiesPanel, ToolConfigPanel, TOOL_TYPES } from '../Panels';
import { CodeModal, BuildModal, CodeEditorModal } from '../Overlays';
import { api, GeneratedProject } from '../../api/client';
import { useKeyboardShortcuts } from '../../hooks/useKeyboardShortcuts';
import type { FunctionToolConfig, AgentSchema, ToolConfig } from '../../types/project';

type FlowPhase = 'idle' | 'input' | 'output';

export function Canvas() {
  const { currentProject, openProject, closeProject, saveProject, selectNode, selectedNodeId, updateAgent: storeUpdateAgent, addAgent, removeAgent, addEdge: addProjectEdge, removeEdge: removeProjectEdge, addToolToAgent, removeToolFromAgent, addSubAgentToContainer, selectedToolId, selectTool, updateToolConfig: storeUpdateToolConfig } = useStore();
  const [showConsole, setShowConsole] = useState(true);
  const [flowPhase, setFlowPhase] = useState<FlowPhase>('idle');
  const [activeAgent, setActiveAgent] = useState<string | null>(null);
  const [iteration, setIteration] = useState(0);
  const [selectedSubAgent, setSelectedSubAgent] = useState<{parent: string, sub: string} | null>(null);
  const [compiledCode, setCompiledCode] = useState<GeneratedProject | null>(null);
  const [buildOutput, setBuildOutput] = useState<{success: boolean, output: string, path: string | null} | null>(null);
  const [building, setBuilding] = useState(false);
  const [builtBinaryPath, setBuiltBinaryPath] = useState<string | null>(null);
  const [showCodeEditor, setShowCodeEditor] = useState(false);

  const [nodes, setNodes, onNodesChange] = useNodesState<Node>([]);
  const [edges, setEdges, onEdgesChange] = useEdgesState<Edge>([]);

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

  useKeyboardShortcuts({ selectedToolId, selectedNodeId, onDeleteTool: removeToolFromAgent, onClearTool: () => selectTool(null) });

  const handleCompile = useCallback(async () => {
    if (!currentProject) return;
    try { setCompiledCode(await api.projects.compile(currentProject.id)); }
    catch (e) { alert('Compile failed: ' + (e as Error).message); }
  }, [currentProject]);

  const handleBuild = useCallback(async () => {
    if (!currentProject) return;
    setBuilding(true);
    setBuildOutput({ success: false, output: '', path: null });
    const eventSource = new EventSource(`/api/projects/${currentProject.id}/build-stream`);
    let output = '';
    eventSource.addEventListener('status', (e) => { output += e.data + '\n'; setBuildOutput({ success: false, output, path: null }); });
    eventSource.addEventListener('output', (e) => { output += e.data + '\n'; setBuildOutput({ success: false, output, path: null }); });
    eventSource.addEventListener('done', (e) => { setBuildOutput({ success: true, output, path: e.data }); setBuiltBinaryPath(e.data); setBuilding(false); eventSource.close(); });
    eventSource.addEventListener('error', (e) => { output += '\nError: ' + ((e as MessageEvent).data || 'Build failed'); setBuildOutput({ success: false, output, path: null }); setBuilding(false); eventSource.close(); });
    eventSource.onerror = () => { setBuilding(false); eventSource.close(); };
  }, [currentProject]);

  const removeSubAgent = useCallback((parentId: string, subId: string) => {
    if (!currentProject) return;
    const parent = currentProject.agents[parentId];
    if (!parent || parent.sub_agents.length <= 1) return;
    updateAgent(parentId, { sub_agents: parent.sub_agents.filter(s => s !== subId) });
    removeAgent(subId);
    setSelectedSubAgent(null);
  }, [currentProject, updateAgent, removeAgent]);

  // Build nodes from project
  useEffect(() => {
    if (!currentProject) return;
    const agentIds = Object.keys(currentProject.agents);
    const allSubAgents = new Set(agentIds.flatMap(id => currentProject.agents[id].sub_agents || []));
    const topLevelAgents = agentIds.filter(id => !allSubAgents.has(id));
    
    const sortedAgents: string[] = [];
    let current = 'START';
    while (sortedAgents.length < topLevelAgents.length) {
      const nextEdge = currentProject.workflow.edges.find(e => e.from === current && e.to !== 'END');
      if (!nextEdge) break;
      if (topLevelAgents.includes(nextEdge.to)) sortedAgents.push(nextEdge.to);
      current = nextEdge.to;
    }
    topLevelAgents.forEach(id => { if (!sortedAgents.includes(id)) sortedAgents.push(id); });

    const newNodes: Node[] = [
      { id: 'START', position: { x: 50, y: 50 }, data: { label: '▶ START' }, type: 'input', style: { background: '#1a472a', border: '2px solid #4ade80', borderRadius: 8, padding: 10, color: '#fff' } },
      { id: 'END', position: { x: 50, y: 150 + sortedAgents.length * 150 }, data: { label: '⏹ END' }, type: 'output', style: { background: '#4a1a1a', border: '2px solid #f87171', borderRadius: 8, padding: 10, color: '#fff' } },
    ];

    sortedAgents.forEach((id, i) => {
      const agent = currentProject.agents[id];
      if (agent.type === 'sequential' || agent.type === 'loop' || agent.type === 'parallel') {
        newNodes.push(buildContainerNode(id, agent, i, currentProject, selectedSubAgent, activeAgent, iteration, setSelectedSubAgent, selectNode, addToolToAgent, removeSubAgent, selectTool, TOOL_TYPES));
      } else if (agent.type === 'router') {
        newNodes.push({ id, type: 'router', position: { x: 50, y: 150 + i * 150 }, data: { label: id, routes: agent.routes || [], isActive: activeAgent === id } });
      } else {
        newNodes.push({ id, type: 'llm', position: { x: 50, y: 150 + i * 150 }, data: { label: id, model: agent.model, tools: agent.tools || [], isActive: activeAgent === id } });
      }
    });
    setNodes(newNodes);
  }, [currentProject, setNodes, selectedSubAgent, removeSubAgent, activeAgent, iteration, selectNode, selectTool, addToolToAgent]);

  // Build edges
  useEffect(() => {
    if (!currentProject) return;
    setEdges(currentProject.workflow.edges.map((e, i) => {
      const animated = (activeAgent && (e.from === activeAgent || e.to === activeAgent)) || (flowPhase === 'input' && e.from === 'START') || (flowPhase === 'output' && e.to === 'END');
      return { id: `e${i}`, source: e.from, target: e.to, type: 'smoothstep', animated, style: { stroke: animated ? '#4ade80' : '#e94560', strokeWidth: animated ? 3 : 2 }, interactionWidth: 20 };
    }));
  }, [currentProject, flowPhase, activeAgent, setEdges]);

  const createAgent = useCallback((agentType: string = 'llm') => {
    if (!currentProject) return;
    const count = Object.keys(currentProject.agents).length;
    const prefix = { sequential: 'seq', loop: 'loop', parallel: 'par', router: 'router' }[agentType] || 'agent';
    const id = `${prefix}_${count + 1}`;

    if (['sequential', 'loop', 'parallel'].includes(agentType)) {
      const sub1 = `${id}_agent_1`, sub2 = `${id}_agent_2`, isLoop = agentType === 'loop';
      addAgent(sub1, { type: 'llm', model: 'gemini-2.0-flash', instruction: isLoop ? 'Process and refine.' : 'Agent 1.', tools: [], sub_agents: [], position: { x: 0, y: 0 } });
      addAgent(sub2, { type: 'llm', model: 'gemini-2.0-flash', instruction: isLoop ? 'Review. Call exit_loop when done.' : 'Agent 2.', tools: isLoop ? ['exit_loop'] : [], sub_agents: [], position: { x: 0, y: 0 } });
      addAgent(id, { type: agentType as 'sequential' | 'loop' | 'parallel', instruction: '', tools: [], sub_agents: [sub1, sub2], position: { x: 50, y: 150 + count * 180 }, max_iterations: isLoop ? 3 : undefined });
    } else if (agentType === 'router') {
      addAgent(id, { type: 'router', model: 'gemini-2.0-flash', instruction: 'Route based on intent.', tools: [], sub_agents: [], position: { x: 50, y: 150 + count * 120 }, routes: [{ condition: 'default', target: 'END' }] });
    } else {
      addAgent(id, { type: 'llm', model: 'gemini-2.0-flash', instruction: 'You are a helpful assistant.', tools: [], sub_agents: [], position: { x: 50, y: 150 + count * 120 } });
    }

    const edgeToEnd = currentProject.workflow.edges.find(e => e.to === 'END');
    if (edgeToEnd) { removeProjectEdge(edgeToEnd.from, 'END'); addProjectEdge(edgeToEnd.from, id); }
    else addProjectEdge('START', id);
    addProjectEdge(id, 'END');
    selectNode(id);
  }, [currentProject, addAgent, addProjectEdge, removeProjectEdge, selectNode]);

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
    if (type) createAgent(type);
  }, [createAgent, selectedNodeId, currentProject, addToolToAgent]);

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
      <MenuBar onExportCode={() => setShowCodeEditor(true)} onNewProject={async () => { const name = prompt('Project name:'); if (name) openProject((await api.projects.create(name)).id); }} />
      <div className="flex flex-1 overflow-hidden">
        <div className="w-48 bg-studio-panel border-r border-gray-700 p-4 flex flex-col overflow-y-auto">
          <AgentPalette onDragStart={onDragStart} onCreate={createAgent} />
          <div className="my-4" />
          <ToolPalette selectedNodeId={selectedNodeId} agentTools={agentTools} onAdd={handleAddTool} onRemove={t => selectedNodeId && removeToolFromAgent(selectedNodeId, t)} />
          <div className="mt-auto space-y-2 pt-4">
            <button onClick={handleCompile} className="w-full px-3 py-2 bg-blue-700 hover:bg-blue-600 rounded text-sm">📄 View Code</button>
            <button onClick={handleBuild} disabled={building} className={`w-full px-3 py-2 rounded text-sm ${building ? 'bg-gray-600' : builtBinaryPath ? 'bg-green-700 hover:bg-green-600' : 'bg-orange-600 hover:bg-orange-500 animate-pulse'}`}>
              {building ? '⏳ Building...' : builtBinaryPath ? '🔨 Build' : '🔨 Build Required'}
            </button>
            <button onClick={() => setShowConsole(!showConsole)} className="w-full px-3 py-2 bg-gray-700 rounded text-sm">{showConsole ? 'Hide Console' : 'Show Console'}</button>
            <button onClick={closeProject} className="w-full px-3 py-2 bg-gray-700 rounded text-sm">Back</button>
          </div>
        </div>

        <div className="flex-1">
          <ReactFlow nodes={nodes} edges={edges} nodeTypes={nodeTypes} onNodesChange={onNodesChange} onEdgesChange={onEdgesChange} onEdgesDelete={onEdgesDelete} onNodesDelete={onNodesDelete} onEdgeDoubleClick={onEdgeDoubleClick} onConnect={onConnect} onNodeClick={onNodeClick} onPaneClick={onPaneClick} onDrop={onDrop} onDragOver={onDragOver} deleteKeyCode={['Backspace', 'Delete']} defaultViewport={{ x: 0, y: 0, zoom: 1 }} minZoom={0.1} maxZoom={2}>
            <Background color="#333" gap={20} />
            <Controls />
          </ReactFlow>
        </div>

        {selectedAgent && selectedNodeId && (
          <PropertiesPanel nodeId={selectedNodeId} agent={selectedAgent} agents={currentProject.agents} toolConfigs={currentProject.tool_configs || {}} onUpdate={updateAgent} onAddSubAgent={() => addSubAgentToContainer(selectedNodeId)} onClose={() => selectNode(null)} onSelectTool={selectTool} onRemoveTool={t => removeToolFromAgent(selectedNodeId, t)} />
        )}
        {selectedToolId && currentProject && (
          <ToolConfigPanel toolId={selectedToolId} config={currentProject.tool_configs?.[selectedToolId] || null} onUpdate={c => updateToolConfig(selectedToolId, c)} onClose={() => selectTool(null)} onOpenCodeEditor={() => setShowCodeEditor(true)} />
        )}
      </div>

      {showConsole && hasAgents && builtBinaryPath && <div className="h-64"><TestConsole onFlowPhase={setFlowPhase} onActiveAgent={setActiveAgent} onIteration={setIteration} binaryPath={builtBinaryPath} /></div>}
      {showConsole && hasAgents && !builtBinaryPath && (
        <div className="h-32 bg-studio-panel border-t border-gray-700 flex items-center justify-center text-gray-500">
          <div className="text-center"><div>Build the project first to test it</div><button onClick={handleBuild} className="mt-2 px-4 py-1 bg-blue-600 hover:bg-blue-700 rounded text-white text-sm">Build Project</button></div>
        </div>
      )}
      {showConsole && !hasAgents && <div className="h-32 bg-studio-panel border-t border-gray-700 flex items-center justify-center text-gray-500">Drag "LLM Agent" onto the canvas to get started</div>}

      {compiledCode && <CodeModal code={compiledCode} onClose={() => setCompiledCode(null)} />}
      {buildOutput && <BuildModal building={building} success={buildOutput.success} output={buildOutput.output} path={buildOutput.path} onClose={() => setBuildOutput(null)} />}
      {showCodeEditor && fnConfig && <CodeEditorModal config={fnConfig} onUpdate={c => updateToolConfig(selectedToolId!, c)} onClose={() => setShowCodeEditor(false)} />}
    </div>
  );
}

// Helper to build container node with sub-agents
function buildContainerNode(id: string, agent: AgentSchema, i: number, project: any, selectedSubAgent: any, activeAgent: string | null, iteration: number, setSelectedSubAgent: any, selectNode: any, addToolToAgent: any, removeSubAgent: any, selectTool: any, TOOL_TYPES: any): Node {
  const isParallel = agent.type === 'parallel', isLoop = agent.type === 'loop';
  const subAgentNodes = (agent.sub_agents || []).map((subId: string, idx: number) => {
    const subAgent = project.agents[subId];
    const isSelected = selectedSubAgent?.parent === id && selectedSubAgent?.sub === subId;
    const isActive = activeAgent === subId;
    return (
      <div key={subId} className={`rounded p-2 cursor-pointer transition-all duration-300 ${isParallel ? '' : idx > 0 ? 'mt-2 border-t border-gray-600 pt-2' : ''} ${isActive ? 'bg-green-900 ring-2 ring-green-400' : isSelected ? 'bg-gray-600 ring-2 ring-blue-400' : 'bg-gray-800 hover:bg-gray-700'}`}
        onClick={(e: React.MouseEvent) => { e.stopPropagation(); setSelectedSubAgent(isSelected ? null : {parent: id, sub: subId}); selectNode(isSelected ? null : subId); }}
        onDragOver={(e: React.DragEvent) => { e.preventDefault(); e.stopPropagation(); }}
        onDrop={(e: React.DragEvent) => { e.preventDefault(); e.stopPropagation(); const t = e.dataTransfer.getData('text/plain'); if (t.startsWith('tool:')) { addToolToAgent(subId, t.slice(5)); setSelectedSubAgent({parent: id, sub: subId}); selectNode(subId); } }}>
        <div className="flex justify-between items-center">
          <span className="text-xs font-medium">{isActive ? '⚡' : isParallel ? '∥' : `${idx + 1}.`} 🤖 {subId}</span>
          {isSelected && agent.sub_agents.length > 1 && <button className="text-red-400 text-xs" onClick={(e: React.MouseEvent) => { e.stopPropagation(); removeSubAgent(id, subId); }}>×</button>}
        </div>
        <div className="text-xs text-gray-400">{isActive ? 'Running...' : 'LLM Agent'}</div>
        {subAgent?.tools?.length > 0 && (
          <div className="border-t border-gray-600 pt-1 mt-1">
            {subAgent.tools.map((t: string) => {
              const base = t.startsWith('function') ? 'function' : t.startsWith('mcp') ? 'mcp' : t;
              const tool = TOOL_TYPES.find((tt: any) => tt.type === base);
              const cfg = project.tool_configs?.[`${subId}_${t}`];
              const name = cfg?.name || tool?.label || t;
              return <div key={t} className="text-xs text-gray-300 px-1 py-0.5 cursor-pointer hover:bg-gray-700" onClick={(e: React.MouseEvent) => { e.stopPropagation(); setSelectedSubAgent({parent: id, sub: subId}); selectNode(subId); selectTool(`${subId}_${t}`); }}>{tool?.icon} {name} <span className="text-blue-400">⚙</span></div>;
            })}
          </div>
        )}
      </div>
    );
  });
  const isLoopActive = isLoop && activeAgent && agent.sub_agents?.includes(activeAgent);
  const configs: Record<string, { icon: string; label: string; bg: string; border: string }> = {
    sequential: { icon: '⛓', label: 'Sequential', bg: '#1e3a5f', border: '#60a5fa' },
    loop: { icon: '🔄', label: isLoopActive ? `Loop (${iteration + 1}/${agent.max_iterations || 3})` : `Loop (${agent.max_iterations || 3}x)`, bg: '#3d1e5f', border: '#a855f7' },
    parallel: { icon: '⚡', label: 'Parallel', bg: '#1e5f3d', border: '#34d399' },
  };
  const cfg = configs[agent.type] || configs.sequential;
  return {
    id, position: { x: 50, y: 150 + i * 150 },
    data: { label: (<div className="text-center min-w-[180px]"><div className="font-semibold">{cfg.icon} {id}</div><div className="text-xs text-gray-400 mb-1">{cfg.label}</div><div className={`border-t border-gray-600 pt-2 mt-1 ${isLoop ? 'relative' : ''}`}>{isLoop && <div className="absolute -left-2 top-0 bottom-0 w-1 border-l-2 border-t-2 border-b-2 border-purple-400 rounded-l" />}{isParallel ? <div className="flex gap-2 flex-wrap justify-center">{subAgentNodes}</div> : <div className={isLoop ? 'ml-1' : ''}>{subAgentNodes}</div>}{isLoop && <div className="absolute -right-2 top-1/2 text-purple-400 text-xs">↩</div>}</div></div>) },
    style: { background: cfg.bg, border: `2px solid ${cfg.border}`, borderRadius: 8, padding: 12, color: '#fff', minWidth: isParallel ? 280 : 200 },
  };
}

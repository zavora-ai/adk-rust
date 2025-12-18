import { useCallback, useEffect, useState, useRef, DragEvent } from 'react';
import {
  ReactFlow,
  Background,
  Controls,
  Node,
  Edge,
  useNodesState,
  useEdgesState,
  Connection,
} from '@xyflow/react';
import '@xyflow/react/dist/style.css';
import Editor from '@monaco-editor/react';
import { useStore } from '../../store';
import { TestConsole } from '../Console/TestConsole';
import { api, GeneratedProject } from '../../api/client';
import type { McpToolConfig, FunctionToolConfig, BrowserToolConfig, FunctionParameter, AgentSchema, ToolConfig } from '../../types/project';

const AGENT_TYPES = [
  { type: 'llm', label: 'LLM Agent', enabled: true },
  { type: 'sequential', label: 'Sequential Agent', enabled: true },
  { type: 'loop', label: 'Loop Agent', enabled: true },
  { type: 'parallel', label: 'Parallel Agent', enabled: true },
  { type: 'router', label: 'Router Agent', enabled: true },
];

const TOOL_TYPES = [
  { type: 'function', label: 'Function Tool', icon: 'ƒ', configurable: true },
  { type: 'mcp', label: 'MCP Tool', icon: '🔌', configurable: true },
  { type: 'browser', label: 'Browser Tool', icon: '🌐', configurable: true },
  { type: 'exit_loop', label: 'Exit Loop', icon: '⏹', configurable: true },
  { type: 'google_search', label: 'Google Search', icon: '🔍', configurable: true },
  { type: 'load_artifact', label: 'Load Artifact', icon: '📦', configurable: true },
];

type FlowPhase = 'idle' | 'input' | 'output';

// Helper to generate full function template
function generateFunctionTemplate(config: FunctionToolConfig): string {
  const fnName = config.name || 'my_function';
  const desc = config.description || 'No description provided';
  const params = config.parameters.map(p => {
    const extractor = p.param_type === 'number' ? 'as_f64().unwrap_or(0.0)' 
      : p.param_type === 'boolean' ? 'as_bool().unwrap_or(false)' 
      : 'as_str().unwrap_or("")';
    return `    let ${p.name} = args["${p.name}"].${extractor};`;
  }).join('\n');
  
  const userCode = config.code || '// Your logic here\nOk(json!({"status": "ok"}))';
  const indentedCode = userCode.split('\n').map(line => line.startsWith('    ') ? line : `    ${line}`).join('\n');
  
  return `/// ${desc}
async fn ${fnName}_fn(_ctx: Arc<dyn ToolContext>, args: Value) -> Result<Value, AdkError> {
    // ═══════════════════════════════════════════════════════════════
    // AUTO-GENERATED: Do not edit above this line
    // Parameters extracted from your schema definitions
    // ═══════════════════════════════════════════════════════════════
${params}

    // ═══════════════════════════════════════════════════════════════
    // YOUR CODE: Edit below this line
    // ═══════════════════════════════════════════════════════════════
${indentedCode}
}`;
}

// Extract user code from full template
function extractUserCode(fullCode: string, config: FunctionToolConfig): string {
  const lines = fullCode.split('\n');
  // Find the "YOUR CODE" marker and extract everything after it until closing }
  const yourCodeIdx = lines.findIndex(l => l.includes('YOUR CODE'));
  if (yourCodeIdx === -1) return config.code || '';
  const startIdx = yourCodeIdx + 2; // Skip marker + separator line
  const endIdx = lines.length - 1; // Exclude closing }
  return lines.slice(startIdx, endIdx).map(l => l.replace(/^    /, '')).join('\n');
}

export function Canvas() {
  const { currentProject, closeProject, saveProject, selectNode, selectedNodeId, updateAgent: storeUpdateAgent, addAgent, removeAgent, addEdge: addProjectEdge, removeEdge: removeProjectEdge, addToolToAgent, removeToolFromAgent, addSubAgentToContainer, selectedToolId, selectTool, updateToolConfig: storeUpdateToolConfig } = useStore();
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

  // Debounced auto-save
  const saveTimerRef = useRef<ReturnType<typeof setTimeout> | null>(null);
  const debouncedSave = useCallback(() => {
    if (saveTimerRef.current) clearTimeout(saveTimerRef.current);
    saveTimerRef.current = setTimeout(() => saveProject(), 500);
  }, [saveProject]);

  // Wrap update functions to invalidate build and auto-save
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
  const handleCompile = useCallback(async () => {
    if (!currentProject) return;
    try {
      const result = await api.projects.compile(currentProject.id);
      setCompiledCode(result);
    } catch (e) {
      alert('Compile failed: ' + (e as Error).message);
    }
  }, [currentProject]);

  const handleBuild = useCallback(async () => {
    if (!currentProject) return;
    setBuilding(true);
    setBuildOutput({ success: false, output: '', path: null });
    
    const eventSource = new EventSource(`/api/projects/${currentProject.id}/build-stream`);
    let output = '';
    
    eventSource.addEventListener('status', (e) => {
      output += e.data + '\n';
      setBuildOutput({ success: false, output, path: null });
    });
    
    eventSource.addEventListener('output', (e) => {
      output += e.data + '\n';
      setBuildOutput({ success: false, output, path: null });
    });
    
    eventSource.addEventListener('done', (e) => {
      setBuildOutput({ success: true, output, path: e.data });
      setBuiltBinaryPath(e.data);
      setBuilding(false);
      eventSource.close();
    });
    
    eventSource.addEventListener('error', (e) => {
      const data = (e as MessageEvent).data || 'Build failed';
      output += '\nError: ' + data;
      setBuildOutput({ success: false, output, path: null });
      setBuilding(false);
      eventSource.close();
    });
    
    eventSource.onerror = () => {
      setBuilding(false);
      eventSource.close();
    };
  }, [currentProject]);

  const removeSubAgent = useCallback((parentId: string, subId: string) => {
    if (!currentProject) return;
    const parent = currentProject.agents[parentId];
    if (!parent || parent.sub_agents.length <= 1) return;
    updateAgent(parentId, { sub_agents: parent.sub_agents.filter(s => s !== subId) });
    removeAgent(subId);
    setSelectedSubAgent(null);
  }, [currentProject, updateAgent, removeAgent]);

  useEffect(() => {
    if (!currentProject) return;
    
    const agentIds = Object.keys(currentProject.agents);
    // Filter out sub-agents (those that belong to a sequential)
    const allSubAgents = new Set(
      agentIds.flatMap(id => currentProject.agents[id].sub_agents || [])
    );
    const topLevelAgents = agentIds.filter(id => !allSubAgents.has(id));
    
    // Sort agents by workflow order (follow edges from START)
    const sortedAgents: string[] = [];
    const edges = currentProject.workflow.edges;
    let current = 'START';
    while (sortedAgents.length < topLevelAgents.length) {
      const nextEdge = edges.find(e => e.from === current && e.to !== 'END');
      if (!nextEdge) break;
      if (topLevelAgents.includes(nextEdge.to)) {
        sortedAgents.push(nextEdge.to);
      }
      current = nextEdge.to;
    }
    // Add any remaining agents not in the chain
    topLevelAgents.forEach(id => {
      if (!sortedAgents.includes(id)) sortedAgents.push(id);
    });
    
    const newNodes: Node[] = [
      { id: 'START', position: { x: 50, y: 50 }, data: { label: '▶ START' }, type: 'input', style: { background: '#1a472a', border: '2px solid #4ade80', borderRadius: 8, padding: 10, color: '#fff' } },
      { id: 'END', position: { x: 50, y: 150 + sortedAgents.length * 150 }, data: { label: '⏹ END' }, type: 'output', style: { background: '#4a1a1a', border: '2px solid #f87171', borderRadius: 8, padding: 10, color: '#fff' } },
    ];
    
    sortedAgents.forEach((id, i) => {
      const agent = currentProject.agents[id];
      if (agent.type === 'sequential' || agent.type === 'loop' || agent.type === 'parallel') {
        const isParallel = agent.type === 'parallel';
        const isLoop = agent.type === 'loop';
        const subAgentNodes = (agent.sub_agents || []).map((subId, idx) => {
          const subAgent = currentProject.agents[subId];
          const isSelected = selectedSubAgent?.parent === id && selectedSubAgent?.sub === subId;
          const isActive = activeAgent === subId;
          const subTools = subAgent?.tools || [];
          return (
            <div 
              key={subId} 
              className={`rounded p-2 cursor-pointer transition-all duration-300 ${isParallel ? '' : idx > 0 ? 'mt-2 border-t border-gray-600 pt-2' : ''} ${isActive ? 'bg-green-900 ring-2 ring-green-400 shadow-lg shadow-green-500/50' : isSelected ? 'bg-gray-600 ring-2 ring-blue-400' : 'bg-gray-800 hover:bg-gray-700'}`}
              onClick={(e) => { e.stopPropagation(); setSelectedSubAgent(isSelected ? null : {parent: id, sub: subId}); selectNode(isSelected ? null : subId); }}
              onDragOver={(e) => { e.preventDefault(); e.stopPropagation(); }}
              onDrop={(e) => {
                e.preventDefault();
                e.stopPropagation();
                const dragData = e.dataTransfer.getData('text/plain');
                const toolType = dragData.startsWith('tool:') ? dragData.slice(5) : '';
                if (toolType && subAgent) {
                  addToolToAgent(subId, toolType);
                  setSelectedSubAgent({parent: id, sub: subId});
                  selectNode(subId);
                }
              }}
            >
              <div className="flex justify-between items-center">
                <span className="text-xs font-medium">{isActive ? '⚡' : (isParallel ? '∥' : `${idx + 1}.`)} 🤖 {subId}</span>
                {isSelected && agent.sub_agents.length > 1 && (
                  <button 
                    className="text-red-400 hover:text-red-300 text-xs"
                    onClick={(e) => { e.stopPropagation(); removeSubAgent(id, subId); }}
                  >×</button>
                )}
              </div>
              <div className="text-xs text-gray-400">{isActive ? 'Running...' : 'LLM Agent'}</div>
              {subTools.length > 0 && (
                <div className="border-t border-gray-600 pt-1 mt-1">
                  {subTools.map(t => {
                    const baseType = t.startsWith('function') ? 'function' : t.startsWith('mcp') ? 'mcp' : t;
                    const tool = TOOL_TYPES.find(tt => tt.type === baseType);
                    const isConfigurable = tool?.configurable;
                    const toolConfigId = `${subId}_${t}`;
                    const toolConfig = currentProject?.tool_configs?.[toolConfigId];
                    let displayName = tool?.label || t;
                    if (baseType === 'function' && toolConfig && 'name' in toolConfig && toolConfig.name) {
                      displayName = toolConfig.name;
                    } else if (baseType === 'mcp') {
                      if (toolConfig && 'name' in toolConfig && toolConfig.name) {
                        displayName = toolConfig.name;
                      } else {
                        const num = t.match(/mcp_(\d+)/)?.[1] || '1';
                        displayName = `MCP Tool ${num}`;
                      }
                    }
                    return (
                      <div 
                        key={t} 
                        className={`text-xs text-gray-300 px-1 py-0.5 rounded ${isConfigurable ? 'cursor-pointer hover:bg-gray-700 hover:text-white' : ''}`}
                        onClick={(e) => {
                          if (isConfigurable) {
                            e.stopPropagation();
                            setSelectedSubAgent({parent: id, sub: subId});
                            selectNode(subId);
                            selectTool(toolConfigId);
                          }
                        }}
                      >
                        {tool?.icon} {displayName} {isConfigurable && <span className="text-blue-400">⚙</span>}
                      </div>
                    );
                  })}
                </div>
              )}
            </div>
          );
        });
        const isLoopActive = agent.type === 'loop' && activeAgent && agent.sub_agents?.includes(activeAgent);
        const config = {
          sequential: { icon: '⛓', label: 'Sequential Agent', bg: '#1e3a5f', border: '#60a5fa' },
          loop: { icon: '🔄', label: isLoopActive ? `Loop Agent (iter ${iteration + 1}/${agent.max_iterations || 3})` : `Loop Agent (${agent.max_iterations || 3}x)`, bg: '#3d1e5f', border: '#a855f7' },
          parallel: { icon: '⚡', label: 'Parallel Agent', bg: '#1e5f3d', border: '#34d399' },
        }[agent.type]!;
        newNodes.push({
          id,
          position: { x: 50, y: 150 + i * 150 },
          data: { 
            label: (
              <div className="text-center min-w-[180px]">
                <div className="font-semibold">{config.icon} {id}</div>
                <div className="text-xs text-gray-400 mb-1">{config.label}</div>
                <div className={`border-t border-gray-600 pt-2 mt-1 ${isLoop ? 'relative' : ''}`}>
                  {isLoop && (
                    <div className="absolute -left-2 top-0 bottom-0 w-1 border-l-2 border-t-2 border-b-2 border-purple-400 rounded-l" />
                  )}
                  {isParallel ? (
                    <div className="flex gap-2 flex-wrap justify-center">{subAgentNodes}</div>
                  ) : (
                    <div className={isLoop ? 'ml-1' : ''}>{subAgentNodes}</div>
                  )}
                  {isLoop && (
                    <div className="absolute -right-2 top-1/2 text-purple-400 text-xs">↩</div>
                  )}
                </div>
              </div>
            )
          },
          style: { background: config.bg, border: `2px solid ${config.border}`, borderRadius: 8, padding: 12, color: '#fff', minWidth: isParallel ? 280 : 200 },
        });
      } else if (agent.type === 'router') {
        const routes = agent.routes || [];
        newNodes.push({
          id,
          position: { x: 50, y: 150 + i * 150 },
          data: { label: (
            <div className="text-center">
              <div className="font-semibold">🔀 {id}</div>
              <div className="text-xs text-gray-400 mb-1">Router Agent</div>
              {routes.length > 0 && (
                <div className="border-t border-gray-600 pt-1 mt-1 text-left">
                  {routes.map((r, idx) => (
                    <div key={idx} className="text-xs text-gray-300">
                      {r.condition} → {r.target}
                    </div>
                  ))}
                </div>
              )}
            </div>
          )},
          style: { background: '#5f4b1e', border: `2px solid ${activeAgent === id ? '#4ade80' : '#fbbf24'}`, borderRadius: 8, padding: 12, color: '#fff', minWidth: 140, boxShadow: activeAgent === id ? '0 0 20px #4ade80' : 'none' },
        });
      } else {
        const tools = agent.tools || [];
        const isActive = activeAgent === id;
        newNodes.push({
          id,
          position: { x: 50, y: 150 + i * 150 },
          data: { label: (
            <div className="text-center">
              <div>{isActive ? '⚡' : '🤖'} {id}</div>
              <div className="text-xs text-gray-400">LLM Agent</div>
              {tools.length > 0 && (
                <div className="border-t border-gray-600 pt-1 mt-1">
                  {tools.map(t => {
                    // Handle function_1, function_2, mcp_1, mcp_2, etc.
                    const baseType = t.startsWith('function') ? 'function' : t.startsWith('mcp') ? 'mcp' : t;
                    const tool = TOOL_TYPES.find(tt => tt.type === baseType);
                    const isConfigurable = tool?.configurable;
                    const toolConfigId = `${id}_${t}`;
                    const toolConfig = currentProject?.tool_configs?.[toolConfigId];
                    // Show friendly name for function/mcp tools
                    let displayName = tool?.label || t;
                    if (baseType === 'function' && toolConfig && 'name' in toolConfig && toolConfig.name) {
                      displayName = toolConfig.name;
                    } else if (baseType === 'mcp') {
                      if (toolConfig && 'name' in toolConfig && toolConfig.name) {
                        displayName = toolConfig.name;
                      } else {
                        const num = t.match(/mcp_(\d+)/)?.[1] || '1';
                        displayName = `MCP Tool ${num}`;
                      }
                    }
                    return (
                      <div 
                        key={t} 
                        className={`text-xs text-gray-300 px-1 py-0.5 rounded ${isConfigurable ? 'cursor-pointer hover:bg-gray-700 hover:text-white' : ''}`}
                        onClick={(e) => {
                          if (isConfigurable) {
                            e.stopPropagation();
                            selectNode(id);
                            selectTool(toolConfigId);
                          }
                        }}
                      >
                        {tool?.icon} {displayName} {isConfigurable && <span className="text-blue-400">⚙</span>}
                      </div>
                    );
                  })}
                </div>
              )}
            </div>
          )},
          style: { background: isActive ? '#1a472a' : '#16213e', border: `2px solid ${isActive ? '#4ade80' : '#e94560'}`, borderRadius: 8, padding: 12, color: '#fff', minWidth: 120, boxShadow: isActive ? '0 0 20px #4ade80' : 'none', transition: 'all 0.3s ease' },
        });
      }
    });
    setNodes(newNodes);
  }, [currentProject, setNodes, selectedSubAgent, removeSubAgent, activeAgent, iteration, selectNode, selectTool]);

  // Handle Delete key for selected tool
  useEffect(() => {
    const handleKeyDown = (e: KeyboardEvent) => {
      if ((e.key === 'Delete' || e.key === 'Backspace') && selectedToolId && selectedNodeId) {
        // Don't delete if focus is in an input/textarea
        const active = document.activeElement;
        if (active?.tagName === 'INPUT' || active?.tagName === 'TEXTAREA') return;
        
        // Extract tool type from selectedToolId (e.g., "agent_1_function_1" -> "function_1")
        const parts = selectedToolId.split('_');
        const toolType = parts.slice(-2).join('_'); // e.g., "function_1" or "mcp_1"
        
        removeToolFromAgent(selectedNodeId, toolType);
        selectTool(null);
        e.preventDefault();
      }
    };
    
    window.addEventListener('keydown', handleKeyDown);
    return () => window.removeEventListener('keydown', handleKeyDown);
  }, [selectedToolId, selectedNodeId, removeToolFromAgent, selectTool]);

  // Update edges based on flow phase and active agent
  useEffect(() => {
    if (!currentProject) return;
    
    const newEdges: Edge[] = currentProject.workflow.edges.map((e, i) => {
      const isStartEdge = e.from === 'START';
      const isEndEdge = e.to === 'END';
      const isActiveEdge = activeAgent && (e.from === activeAgent || e.to === activeAgent);
      const animated = isActiveEdge || (flowPhase === 'input' && isStartEdge) || (flowPhase === 'output' && isEndEdge);
      
      return {
        id: `e${i}`,
        source: e.from,
        target: e.to,
        type: 'smoothstep',
        animated,
        style: { stroke: animated ? '#4ade80' : '#e94560', strokeWidth: animated ? 3 : 2 },
        interactionWidth: 20,
      };
    });
    setEdges(newEdges);
  }, [currentProject, flowPhase, activeAgent, setEdges]);

  const createAgent = useCallback((agentType: string = 'llm') => {
    if (!currentProject) return;
    const agentCount = Object.keys(currentProject.agents).length;
    const prefix = { sequential: 'seq', loop: 'loop', parallel: 'par', router: 'router' }[agentType] || 'agent';
    const id = `${prefix}_${agentCount + 1}`;
    
    if (agentType === 'sequential' || agentType === 'loop' || agentType === 'parallel') {
      // Create container with 2 default sub-agents
      const sub1 = `${id}_agent_1`;
      const sub2 = `${id}_agent_2`;
      const isLoop = agentType === 'loop';
      addAgent(sub1, {
        type: 'llm',
        model: 'gemini-2.0-flash',
        instruction: isLoop ? 'Process and refine the content.' : 'You are agent 1.',
        tools: [],
        sub_agents: [],
        position: { x: 0, y: 0 },
      });
      addAgent(sub2, {
        type: 'llm',
        model: 'gemini-2.0-flash',
        instruction: isLoop ? 'Answer user. Review and improve only if necessary. Call exit_loop when done.' : 'You are agent 2.',
        tools: isLoop ? ['exit_loop'] : [],
        sub_agents: [],
        position: { x: 0, y: 0 },
      });
      addAgent(id, {
        type: agentType as 'sequential' | 'loop' | 'parallel',
        instruction: '',
        tools: [],
        sub_agents: [sub1, sub2],
        position: { x: 50, y: 150 + agentCount * 180 },
        max_iterations: agentType === 'loop' ? 3 : undefined,
      });
    } else if (agentType === 'router') {
      addAgent(id, {
        type: 'router',
        model: 'gemini-2.0-flash',
        instruction: 'Route the request based on intent.',
        tools: [],
        sub_agents: [],
        position: { x: 50, y: 150 + agentCount * 120 },
        routes: [
          { condition: 'default', target: 'END' },
        ],
      });
    } else {
      addAgent(id, {
        type: 'llm',
        model: 'gemini-2.0-flash',
        instruction: 'You are a helpful assistant.',
        tools: [],
        sub_agents: [],
        position: { x: 50, y: 150 + agentCount * 120 },
      });
    }
    
    // Insert new agent into the chain: find what points to END, insert before it
    const edges = currentProject.workflow.edges;
    const edgeToEnd = edges.find(e => e.to === 'END');
    
    if (edgeToEnd) {
      // Remove old edge to END, insert new agent in between
      removeProjectEdge(edgeToEnd.from, 'END');
      addProjectEdge(edgeToEnd.from, id);
      addProjectEdge(id, 'END');
    } else {
      // No edges yet, connect START → agent → END
      addProjectEdge('START', id);
      addProjectEdge(id, 'END');
    }
    
    selectNode(id);
  }, [currentProject, addAgent, addProjectEdge, removeProjectEdge, selectNode]);

  const onDragStart = (e: DragEvent, nodeType: string) => {
    e.dataTransfer.setData('application/reactflow', nodeType);
    e.dataTransfer.effectAllowed = 'move';
  };

  const onDragOver = useCallback((e: DragEvent) => {
    e.preventDefault();
    // Match dropEffect to what's being dragged
    if (e.dataTransfer.types.includes('text/plain')) {
      e.dataTransfer.dropEffect = 'copy';  // tools
    } else {
      e.dataTransfer.dropEffect = 'move';  // agents
    }
  }, []);

  const onDrop = useCallback((e: DragEvent) => {
    e.preventDefault();
    e.stopPropagation();
    console.log('onDrop fired');
    
    // Check if dropping a tool
    const dragData = e.dataTransfer.getData('text/plain');
    const toolType = dragData.startsWith('tool:') ? dragData.slice(5) : '';
    console.log('onDrop - dragData:', dragData, 'toolType:', toolType, 'selectedNodeId:', selectedNodeId);
    if (toolType) {
      // Find node at drop point using DOM
      const target = e.target as HTMLElement;
      console.log('drop target:', target, 'className:', target.className);
      const nodeElement = target.closest('[data-id]');
      let nodeId = nodeElement?.getAttribute('data-id');
      console.log('nodeElement:', nodeElement, 'nodeId:', nodeId);
      
      // Also try elementsFromPoint as fallback
      if (!nodeId || nodeId === 'START' || nodeId === 'END') {
        const elements = document.elementsFromPoint(e.clientX, e.clientY);
        console.log('elementsFromPoint:', elements.map(el => ({ tag: el.tagName, class: el.className, dataId: (el as HTMLElement).closest('[data-id]')?.getAttribute('data-id') })));
        for (const el of elements) {
          const node = (el as HTMLElement).closest('[data-id]');
          const id = node?.getAttribute('data-id');
          if (id && id !== 'START' && id !== 'END' && currentProject?.agents[id]) {
            nodeId = id;
            break;
          }
        }
      }
      
      // Fall back to selected node if no drop target found
      if ((!nodeId || !currentProject?.agents[nodeId]) && selectedNodeId && currentProject?.agents[selectedNodeId]) {
        console.log('falling back to selectedNodeId:', selectedNodeId);
        nodeId = selectedNodeId;
      }
      
      console.log('final nodeId:', nodeId, 'exists:', !!currentProject?.agents[nodeId || '']);
      if (nodeId && nodeId !== 'START' && nodeId !== 'END' && currentProject?.agents[nodeId]) {
        addToolToAgent(nodeId, toolType);
        selectNode(nodeId);
        if (TOOL_TYPES.find(t => t.type === toolType)?.configurable) {
          const agentTools = currentProject?.agents[nodeId]?.tools || [];
          let newToolId: string;
          if (toolType === 'function') {
            const count = agentTools.filter(t => t.startsWith('function')).length;
            newToolId = `${nodeId}_function_${count + 1}`;
          } else if (toolType === 'mcp') {
            const count = agentTools.filter(t => t.startsWith('mcp')).length;
            newToolId = `${nodeId}_mcp_${count + 1}`;
          } else {
            newToolId = `${nodeId}_${toolType}`;
          }
          selectTool(newToolId);
        }
      }
      return;
    }
    
    // Otherwise, creating an agent
    const type = e.dataTransfer.getData('application/reactflow');
    console.log('agent drop - type:', type);
    if (!type) return;
    createAgent(type);
  }, [createAgent, nodes, addToolToAgent, selectNode, selectTool, currentProject]);

  const onConnect = useCallback((params: Connection) => {
    if (params.source && params.target) {
      addProjectEdge(params.source, params.target);
    }
  }, [addProjectEdge]);

  const onEdgesDelete = useCallback((edgesToDelete: Edge[]) => {
    edgesToDelete.forEach((edge) => {
      removeProjectEdge(edge.source, edge.target);
    });
  }, [removeProjectEdge]);

  const onNodesDelete = useCallback((nodesToDelete: Node[]) => {
    nodesToDelete.forEach((node) => {
      if (node.id !== 'START' && node.id !== 'END') {
        removeAgent(node.id);
      }
    });
  }, [removeAgent]);

  const onEdgeDoubleClick = useCallback((_: React.MouseEvent, edge: Edge) => {
    removeProjectEdge(edge.source, edge.target);
  }, [removeProjectEdge]);

  const onNodeClick = useCallback((_: React.MouseEvent, node: Node) => {
    if (node.id !== 'START' && node.id !== 'END') {
      selectNode(node.id);
    } else {
      selectNode(null);
    }
  }, [selectNode]);

  const onPaneClick = useCallback(() => selectNode(null), [selectNode]);

  if (!currentProject) return null;

  const selectedAgent = selectedNodeId ? currentProject.agents[selectedNodeId] : null;
  const hasAgents = Object.keys(currentProject.agents).length > 0;

  return (
    <div className="flex flex-col h-full">
      <div className="flex flex-1 overflow-hidden">
        {/* Palette */}
        <div className="w-48 bg-studio-panel border-r border-gray-700 p-4 flex flex-col overflow-y-auto">
          <h3 className="font-semibold mb-2">Agents</h3>
          <div className="space-y-1 mb-4">
            {AGENT_TYPES.map(({ type, label }) => (
              <div
                key={type}
                draggable
                onDragStart={(e) => onDragStart(e, type)}
                onClick={() => createAgent(type)}
                className="p-2 bg-studio-accent rounded text-sm cursor-grab hover:bg-studio-highlight"
              >
                ⊕ {label}
              </div>
            ))}
          </div>
          
          <h3 className="font-semibold mb-2">Tools</h3>
          <div className="space-y-1 flex-1">
            {TOOL_TYPES.map(({ type, label, icon, configurable }) => {
              const agentTools = selectedNodeId ? currentProject?.agents[selectedNodeId]?.tools || [] : [];
              // For function/mcp tools, check if any exists; for others, exact match
              const isMultiTool = type === 'function' || type === 'mcp';
              const isAdded = isMultiTool
                ? agentTools.some(t => t.startsWith(type))
                : agentTools.includes(type);
              const toolCount = isMultiTool ? agentTools.filter(t => t.startsWith(type)).length : 0;
              return (
                <div
                  key={type}
                  draggable
                  onDragStart={(e) => {
                    e.dataTransfer.setData('text/plain', `tool:${type}`);
                    e.dataTransfer.effectAllowed = 'copy';
                  }}
                  className={`p-2 rounded text-sm cursor-grab flex items-center gap-2 ${
                    isAdded ? 'bg-green-800 hover:bg-green-700' : 'bg-gray-700 hover:bg-gray-600'
                  } ${!selectedNodeId ? 'opacity-50' : ''}`}
                  onClick={() => {
                    if (!selectedNodeId) return;
                    // Function and MCP tools can always be added (multiple allowed)
                    if (isMultiTool) {
                      addToolToAgent(selectedNodeId, type);
                      const newToolId = `${selectedNodeId}_${type}_${toolCount + 1}`;
                      if (configurable) setTimeout(() => selectTool(newToolId), 0);
                    } else if (isAdded) {
                      removeToolFromAgent(selectedNodeId, type);
                    } else {
                      addToolToAgent(selectedNodeId, type);
                      if (configurable) setTimeout(() => selectTool(`${selectedNodeId}_${type}`), 0);
                    }
                  }}
                >
                  <span>{icon}</span>
                  <span className="text-xs">{label}</span>
                  {isMultiTool && toolCount > 0 && <span className="ml-auto text-xs bg-blue-600 px-1 rounded">{toolCount}</span>}
                  {!isMultiTool && isAdded && <span className="ml-auto text-xs">✓</span>}
                </div>
              );
            })}
          </div>
          <div className="space-y-2">
            <button onClick={handleCompile} className="w-full px-3 py-2 bg-blue-700 hover:bg-blue-600 rounded text-sm">
              📄 View Code
            </button>
            <button onClick={handleBuild} disabled={building} className={`w-full px-3 py-2 rounded text-sm ${building ? 'bg-gray-600' : builtBinaryPath ? 'bg-green-700 hover:bg-green-600' : 'bg-orange-600 hover:bg-orange-500 animate-pulse'}`}>
              {building ? '⏳ Building...' : builtBinaryPath ? '🔨 Build' : '🔨 Build Required'}
            </button>
            <button onClick={() => setShowConsole(!showConsole)} className="w-full px-3 py-2 bg-gray-700 rounded text-sm">
              {showConsole ? 'Hide Console' : 'Show Console'}
            </button>
            <button onClick={closeProject} className="w-full px-3 py-2 bg-gray-700 rounded text-sm">Back</button>
          </div>
        </div>

        {/* Canvas */}
        <div className="flex-1">
          <ReactFlow
            nodes={nodes}
            edges={edges}
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
          >
            <Background color="#333" gap={20} />
            <Controls />
          </ReactFlow>
        </div>

        {/* Properties */}
        {selectedAgent && (
          <div className="w-72 bg-studio-panel border-l border-gray-700 p-4 overflow-y-auto">
            <div className="flex justify-between items-center mb-4">
              <h3 className="font-semibold">{selectedNodeId}</h3>
              <button onClick={() => selectNode(null)} className="px-2 py-1 bg-gray-600 rounded text-xs">Close</button>
            </div>
            
            {(selectedAgent.type === 'sequential' || selectedAgent.type === 'loop' || selectedAgent.type === 'parallel') ? (
              /* Container Agent Properties */
              <div>
                {selectedAgent.type === 'loop' && (
                  <>
                    <div className="mb-4 p-2 bg-purple-900/50 border border-purple-600 rounded text-xs">
                      <div className="font-semibold text-purple-400 mb-1">💡 Loop Agent Tips</div>
                      <p className="text-purple-200">Sub-agents run repeatedly until max iterations or exit_loop tool is called.</p>
                      <p className="text-purple-200 mt-1">Add the <span className="font-mono bg-purple-800 px-1 rounded">exit_loop</span> tool to let the agent decide when to stop.</p>
                    </div>
                    <div className="mb-4">
                      <label className="block text-sm text-gray-400 mb-1">Max Iterations</label>
                      <input
                        type="number"
                        min="1"
                        className="w-full px-2 py-1 bg-studio-bg border border-gray-600 rounded text-sm"
                        value={selectedAgent.max_iterations || 3}
                        onChange={(e) => updateAgent(selectedNodeId!, { max_iterations: parseInt(e.target.value) || 3 })}
                      />
                    </div>
                  </>
                )}
                <label className="block text-sm text-gray-400 mb-2">
                  Sub-Agents {selectedAgent.type === 'parallel' ? '(run concurrently)' : '(in order)'}
                </label>
                {(selectedAgent.sub_agents || []).map((subId, idx) => {
                  const subAgent = currentProject.agents[subId];
                  if (!subAgent) return null;
                  return (
                    <div key={subId} className="mb-4 p-2 bg-gray-800 rounded">
                      <div className="text-sm font-medium mb-2">{selectedAgent.type === 'parallel' ? '∥' : `${idx + 1}.`} {subId}</div>
                      <label className="block text-xs text-gray-400 mb-1">Model</label>
                      <input
                        className="w-full px-2 py-1 bg-studio-bg border border-gray-600 rounded text-xs mb-2"
                        value={subAgent.model || ''}
                        onChange={(e) => updateAgent(subId, { model: e.target.value })}
                      />
                      <label className="block text-xs text-gray-400 mb-1">Instruction</label>
                      <textarea
                        className="w-full px-2 py-1 bg-studio-bg border border-gray-600 rounded text-xs h-20"
                        placeholder={selectedAgent.type === 'loop' ? 'Refine the content iteratively. When satisfied, call exit_loop to finish.' : ''}
                        value={subAgent.instruction}
                        onChange={(e) => updateAgent(subId, { instruction: e.target.value })}
                      />
                    </div>
                  );
                })}
                <button
                  onClick={() => addSubAgentToContainer(selectedNodeId!)}
                  className="w-full py-2 bg-gray-700 hover:bg-gray-600 rounded text-sm"
                >
                  + Add Sub-Agent
                </button>
              </div>
            ) : selectedAgent.type === 'router' ? (
              /* Router Agent Properties */
              <div>
                <label className="block text-sm text-gray-400 mb-1">Model</label>
                <input
                  className="w-full px-2 py-1 bg-studio-bg border border-gray-600 rounded text-sm mb-3"
                  value={selectedAgent.model || ''}
                  onChange={(e) => updateAgent(selectedNodeId!, { model: e.target.value })}
                />
                <label className="block text-sm text-gray-400 mb-1">Routing Instruction</label>
                <textarea
                  className="w-full px-2 py-1 bg-studio-bg border border-gray-600 rounded text-sm h-20 mb-3"
                  placeholder="Analyze the request and decide which agent to route to..."
                  value={selectedAgent.instruction}
                  onChange={(e) => updateAgent(selectedNodeId!, { instruction: e.target.value })}
                />
                <label className="block text-sm text-gray-400 mb-2">Routes</label>
                {(selectedAgent.routes || []).map((route, idx) => (
                  <div key={idx} className="flex gap-1 mb-2 items-center">
                    <input
                      className="flex-1 px-2 py-1 bg-studio-bg border border-gray-600 rounded text-xs"
                      placeholder="condition"
                      value={route.condition}
                      onChange={(e) => {
                        const routes = [...(selectedAgent.routes || [])];
                        routes[idx] = { ...route, condition: e.target.value };
                        updateAgent(selectedNodeId!, { routes });
                      }}
                    />
                    <span className="text-gray-500">→</span>
                    <input
                      className="flex-1 px-2 py-1 bg-studio-bg border border-gray-600 rounded text-xs"
                      placeholder="target"
                      value={route.target}
                      onChange={(e) => {
                        const routes = [...(selectedAgent.routes || [])];
                        routes[idx] = { ...route, target: e.target.value };
                        updateAgent(selectedNodeId!, { routes });
                      }}
                    />
                    <button
                      className="text-red-400 hover:text-red-300 text-sm"
                      onClick={() => {
                        const routes = (selectedAgent.routes || []).filter((_, i) => i !== idx);
                        updateAgent(selectedNodeId!, { routes });
                      }}
                    >×</button>
                  </div>
                ))}
                <button
                  className="w-full py-1 bg-gray-700 hover:bg-gray-600 rounded text-xs"
                  onClick={() => {
                    const routes = [...(selectedAgent.routes || []), { condition: '', target: '' }];
                    updateAgent(selectedNodeId!, { routes });
                  }}
                >+ Add Route</button>
              </div>
            ) : (
              /* LLM Agent Properties */
              <div>
                <label className="block text-sm text-gray-400 mb-1">Model</label>
                <input
                  className="w-full px-2 py-1 bg-studio-bg border border-gray-600 rounded text-sm mb-3"
                  value={selectedAgent.model || ''}
                  onChange={(e) => updateAgent(selectedNodeId!, { model: e.target.value })}
                />
                <label className="block text-sm text-gray-400 mb-1">Instruction</label>
                <textarea
                  className="w-full px-2 py-1 bg-studio-bg border border-gray-600 rounded text-sm h-24"
                  value={selectedAgent.instruction}
                  onChange={(e) => updateAgent(selectedNodeId!, { instruction: e.target.value })}
                />
                {selectedAgent.tools.length > 0 && (
                  <div className="mt-3">
                    <label className="block text-sm text-gray-400 mb-1">Tools</label>
                    <div className="flex flex-wrap gap-1">
                      {selectedAgent.tools.map(t => {
                        const baseType = t.startsWith('function') ? 'function' : t.startsWith('mcp') ? 'mcp' : t;
                        const tool = TOOL_TYPES.find(tt => tt.type === baseType);
                        const isConfigurable = tool?.configurable;
                        const toolId = `${selectedNodeId}_${t}`;
                        const toolConfig = currentProject?.tool_configs?.[toolId];
                        let displayName = tool?.label || t;
                        if (baseType === 'function' && toolConfig && 'name' in toolConfig && toolConfig.name) {
                          displayName = toolConfig.name;
                        } else if (baseType === 'mcp') {
                          if (toolConfig && 'name' in toolConfig && toolConfig.name) {
                            displayName = toolConfig.name;
                          } else {
                            const num = t.match(/mcp_(\d+)/)?.[1] || '1';
                            displayName = `MCP Tool ${num}`;
                          }
                        }
                        return (
                          <span 
                            key={t} 
                            className={`text-xs px-2 py-1 rounded flex items-center gap-1 ${toolConfig ? 'bg-green-800' : 'bg-gray-700'} ${isConfigurable ? 'cursor-pointer hover:bg-gray-600' : ''}`}
                            onClick={() => isConfigurable && selectTool(toolId)}
                          >
                            {tool?.icon} {displayName}
                            {isConfigurable && <span className="text-blue-400">⚙</span>}
                            <button onClick={(e) => { e.stopPropagation(); removeToolFromAgent(selectedNodeId!, t); }} className="ml-1 text-red-400 hover:text-red-300">×</button>
                          </span>
                        );
                      })}
                    </div>
                  </div>
                )}
              </div>
            )}
          </div>
        )}

        {/* Tool Configuration Panel */}
        {selectedToolId && currentProject && (() => {
          const actualToolType = selectedToolId.includes('_mcp') ? 'mcp' 
            : selectedToolId.includes('_function') ? 'function' 
            : selectedToolId.includes('_browser') ? 'browser'
            : selectedToolId.includes('_exit_loop') ? 'exit_loop'
            : selectedToolId.includes('_google_search') ? 'google_search'
            : selectedToolId.includes('_load_artifact') ? 'load_artifact'
            : '';
          const config = currentProject.tool_configs?.[selectedToolId];
          
          const getDefaultConfig = (type: string) => {
            if (type === 'mcp') return { type: 'mcp', server_command: '', server_args: [], tool_filter: [] } as McpToolConfig;
            if (type === 'function') return { type: 'function', name: '', description: '', parameters: [] } as FunctionToolConfig;
            if (type === 'browser') return { type: 'browser', headless: true, timeout_ms: 30000 } as BrowserToolConfig;
            return null;
          };
          
          const currentConfig = config || getDefaultConfig(actualToolType);
          // For simple tools without config, still show the panel
          const isSimpleTool = ['exit_loop', 'google_search', 'load_artifact'].includes(actualToolType);
          if (!currentConfig && !isSimpleTool) return null;
          
          return (
            <div className="w-80 bg-studio-panel border-l border-gray-700 p-4 overflow-y-auto">
              <div className="flex justify-between items-center mb-4">
                <h3 className="font-semibold">Configure Tool</h3>
                <button onClick={() => selectTool(null)} className="px-2 py-1 bg-gray-600 rounded text-xs">Close</button>
              </div>
              
              {actualToolType === 'mcp' && (() => {
                const mcpConfig = currentConfig as McpToolConfig;
                const mcpTemplates = [
                  { name: 'Time', icon: '🕐', command: 'uvx', args: ['mcp-server-time'], desc: 'Get current time, convert timezones' },
                  { name: 'Fetch', icon: '🌐', command: 'uvx', args: ['mcp-server-fetch'], desc: 'Fetch URLs and extract content' },
                  { name: 'Filesystem', icon: '📁', command: 'npx', args: ['-y', '@modelcontextprotocol/server-filesystem', '/tmp'], desc: 'Read/write files' },
                  { name: 'GitHub', icon: '🐙', command: 'npx', args: ['-y', '@modelcontextprotocol/server-github'], desc: 'GitHub API (needs GITHUB_TOKEN)' },
                  { name: 'Postgres', icon: '🐘', command: 'npx', args: ['-y', '@modelcontextprotocol/server-postgres', 'postgresql://localhost/db'], desc: 'Query PostgreSQL' },
                  { name: 'SQLite', icon: '💾', command: 'uvx', args: ['mcp-server-sqlite', '--db-path', '/tmp/data.db'], desc: 'Query SQLite database' },
                  { name: 'Brave Search', icon: '🔍', command: 'npx', args: ['-y', '@modelcontextprotocol/server-brave-search'], desc: 'Web search (needs BRAVE_API_KEY)' },
                  { name: 'Memory', icon: '🧠', command: 'npx', args: ['-y', '@modelcontextprotocol/server-memory'], desc: 'Persistent key-value memory' },
                ];
                return (
                  <div className="space-y-3">
                    <div>
                      <label className="block text-sm text-gray-400 mb-2">Quick Templates</label>
                      <div className="grid grid-cols-2 gap-1 mb-3">
                        {mcpTemplates.map((t) => (
                          <button
                            key={t.name}
                            className="flex items-center gap-1 px-2 py-1 bg-gray-700 hover:bg-gray-600 rounded text-xs text-left"
                            title={t.desc}
                            onClick={() => updateToolConfig(selectedToolId, { ...mcpConfig, name: t.name, server_command: t.command, server_args: t.args })}
                          >
                            <span>{t.icon}</span>
                            <span className="truncate">{t.name}</span>
                          </button>
                        ))}
                      </div>
                    </div>
                    <div>
                      <label className="block text-sm text-gray-400 mb-1">Server Command</label>
                      <input
                        className="w-full px-2 py-1 bg-studio-bg border border-gray-600 rounded text-sm"
                        placeholder="npx, uvx, node..."
                        value={mcpConfig.server_command}
                        onChange={(e) => updateToolConfig(selectedToolId, { ...mcpConfig, server_command: e.target.value })}
                      />
                    </div>
                    <div>
                      <label className="block text-sm text-gray-400 mb-1">Server Args (one per line)</label>
                      <textarea
                        className="w-full px-2 py-1 bg-studio-bg border border-gray-600 rounded text-sm h-20"
                        placeholder="-m&#10;mcp_server_time"
                        value={mcpConfig.server_args.join('\n')}
                        onChange={(e) => updateToolConfig(selectedToolId, { ...mcpConfig, server_args: e.target.value.split('\n').filter(Boolean) })}
                      />
                    </div>
                    <div>
                      <label className="block text-sm text-gray-400 mb-1">Tool Filter (optional, one per line)</label>
                      <textarea
                        className="w-full px-2 py-1 bg-studio-bg border border-gray-600 rounded text-sm h-16"
                        placeholder="get_time&#10;list_files"
                        value={(mcpConfig.tool_filter || []).join('\n')}
                        onChange={(e) => updateToolConfig(selectedToolId, { ...mcpConfig, tool_filter: e.target.value.split('\n').filter(Boolean) })}
                      />
                    </div>
                  </div>
                );
              })()}
              
              {actualToolType === 'function' && (() => {
                const fnConfig = currentConfig as FunctionToolConfig;
                return (
                  <div className="space-y-3">
                    <div>
                      <label className="block text-sm text-gray-400 mb-1">Function Name</label>
                      <input
                        className="w-full px-2 py-1 bg-studio-bg border border-gray-600 rounded text-sm"
                        placeholder="get_weather"
                        value={fnConfig.name}
                        onChange={(e) => updateToolConfig(selectedToolId, { ...fnConfig, name: e.target.value })}
                      />
                    </div>
                    <div>
                      <label className="block text-sm text-gray-400 mb-1">Description</label>
                      <textarea
                        className="w-full px-2 py-1 bg-studio-bg border border-gray-600 rounded text-sm h-28"
                        placeholder="Gets current weather for a location"
                        value={fnConfig.description}
                        onChange={(e) => updateToolConfig(selectedToolId, { ...fnConfig, description: e.target.value })}
                      />
                    </div>
                    <div>
                      <label className="block text-sm text-gray-400 mb-1">Parameters</label>
                      {fnConfig.parameters.map((param, idx) => (
                        <div key={idx} className="flex gap-1 mb-1 items-center">
                          <input
                            className="flex-1 px-1 py-0.5 bg-studio-bg border border-gray-600 rounded text-xs"
                            placeholder="name"
                            value={param.name}
                            onChange={(e) => {
                              const params = [...fnConfig.parameters];
                              params[idx] = { ...param, name: e.target.value };
                              updateToolConfig(selectedToolId, { ...fnConfig, parameters: params });
                            }}
                          />
                          <select
                            className="px-1 py-0.5 bg-studio-bg border border-gray-600 rounded text-xs"
                            value={param.param_type}
                            onChange={(e) => {
                              const params = [...fnConfig.parameters];
                              params[idx] = { ...param, param_type: e.target.value as 'string' | 'number' | 'boolean' };
                              updateToolConfig(selectedToolId, { ...fnConfig, parameters: params });
                            }}
                          >
                            <option value="string">string</option>
                            <option value="number">number</option>
                            <option value="boolean">boolean</option>
                          </select>
                          <label className="text-xs flex items-center gap-1">
                            <input
                              type="checkbox"
                              checked={param.required}
                              onChange={(e) => {
                                const params = [...fnConfig.parameters];
                                params[idx] = { ...param, required: e.target.checked };
                                updateToolConfig(selectedToolId, { ...fnConfig, parameters: params });
                              }}
                            />
                            req
                          </label>
                          <button
                            className="text-red-400 hover:text-red-300 text-xs"
                            onClick={() => {
                              const params = fnConfig.parameters.filter((_, i) => i !== idx);
                              updateToolConfig(selectedToolId, { ...fnConfig, parameters: params });
                            }}
                          >×</button>
                        </div>
                      ))}
                      <button
                        className="w-full py-1 bg-gray-700 hover:bg-gray-600 rounded text-xs mt-1"
                        onClick={() => {
                          const newParam: FunctionParameter = { name: '', param_type: 'string', description: '', required: false };
                          updateToolConfig(selectedToolId, { ...fnConfig, parameters: [...fnConfig.parameters, newParam] });
                        }}
                      >+ Add Parameter</button>
                    </div>
                    <div>
                      <div className="flex justify-between items-center mb-1">
                        <label className="text-sm text-gray-400">Code (Rust)</label>
                        <button
                          className="text-xs text-blue-400 hover:text-blue-300"
                          onClick={() => {
                            const template = generateFunctionTemplate(fnConfig);
                            (window as any).__codeEditorModal = { fnConfig, template, selectedToolId, updateToolConfig };
                            setShowCodeEditor(true);
                          }}
                        >✏️ Expand to Edit</button>
                      </div>
                      <textarea
                        className="w-full px-2 py-2 bg-gray-900 border border-gray-600 rounded text-xs font-mono h-40 text-gray-400 cursor-pointer"
                        value={generateFunctionTemplate(fnConfig)}
                        readOnly
                        onClick={() => setShowCodeEditor(true)}
                      />
                    </div>
                    <div>
                      <label className="block text-sm text-gray-400 mb-2">Quick Templates</label>
                      <div className="grid grid-cols-2 gap-1">
                        {[
                          { name: 'HTTP Request', icon: '🌐', template: {
                            name: 'http_request',
                            description: 'Make HTTP requests to external APIs',
                            parameters: [
                              { name: 'url', param_type: 'string' as const, description: 'URL to request', required: true },
                              { name: 'method', param_type: 'string' as const, description: 'GET, POST, PUT, DELETE', required: false },
                              { name: 'body', param_type: 'string' as const, description: 'Request body (JSON)', required: false },
                            ],
                            code: `let client = reqwest::Client::new();
let mut req = match method {
    "POST" => client.post(url),
    "PUT" => client.put(url),
    "DELETE" => client.delete(url),
    _ => client.get(url),
};
req = req.header("User-Agent", "ADK-Agent/1.0");
if !body.is_empty() {
    req = req.header("Content-Type", "application/json").body(body.to_string());
}
let resp = req.send().await.map_err(|e| adk_core::AdkError::Tool(e.to_string()))?;
let text = resp.text().await.map_err(|e| adk_core::AdkError::Tool(e.to_string()))?;
Ok(json!({"response": text}))`
                          }},
                          { name: 'Send Email', icon: '📧', template: {
                            name: 'send_email',
                            description: 'Send an email via SMTP (Gmail, Outlook, etc)',
                            parameters: [
                              { name: 'to', param_type: 'string' as const, description: 'Recipient email', required: true },
                              { name: 'subject', param_type: 'string' as const, description: 'Email subject', required: true },
                              { name: 'body', param_type: 'string' as const, description: 'Email body', required: true },
                            ],
                            code: `use lettre::{Message, SmtpTransport, Transport};
use lettre::transport::smtp::authentication::Credentials;

let smtp_user = std::env::var("SMTP_USER").unwrap_or_default();
let smtp_pass = std::env::var("SMTP_PASS").unwrap_or_default();
let smtp_host = std::env::var("SMTP_HOST").unwrap_or("smtp.gmail.com".to_string());

if smtp_user.is_empty() || smtp_pass.is_empty() {
    return Ok(json!({"error": "Set SMTP_USER and SMTP_PASS environment variables"}));
}

let email = Message::builder()
    .from(smtp_user.parse().map_err(|e| adk_core::AdkError::Tool(format!("Invalid from: {}", e)))?)
    .to(to.parse().map_err(|e| adk_core::AdkError::Tool(format!("Invalid to: {}", e)))?)
    .subject(subject)
    .body(body.to_string())
    .map_err(|e| adk_core::AdkError::Tool(e.to_string()))?;

let creds = Credentials::new(smtp_user.clone(), smtp_pass);
let mailer = SmtpTransport::relay(&smtp_host)
    .map_err(|e| adk_core::AdkError::Tool(e.to_string()))?
    .credentials(creds)
    .build();

mailer.send(&email).map_err(|e| adk_core::AdkError::Tool(e.to_string()))?;
Ok(json!({"status": "sent", "to": to, "subject": subject}))`
                          }},
                          { name: 'Read File', icon: '📄', template: {
                            name: 'read_file',
                            description: 'Read file from session workspace (isolated per session)',
                            parameters: [
                              { name: 'path', param_type: 'string' as const, description: 'File path relative to session workspace', required: true },
                              { name: 'max_bytes', param_type: 'number' as const, description: 'Max bytes to read (default 1MB)', required: false },
                            ],
                            code: `// Session-isolated workspace
let session_id = std::env::args().nth(1).unwrap_or_else(|| "default".to_string());
let workspace = std::path::PathBuf::from(format!("/tmp/adk-workspace/{}", session_id));
std::fs::create_dir_all(&workspace).ok();

let full_path = workspace.join(path.trim_start_matches('/'));
let max = if max_bytes > 0.0 { max_bytes as u64 } else { 1_000_000 };

if !full_path.exists() {
    return Ok(json!({"error": "File not found", "path": path, "workspace": workspace.display().to_string()}));
}

let meta = std::fs::metadata(&full_path)
    .map_err(|e| adk_core::AdkError::Tool(e.to_string()))?;

if meta.is_dir() {
    let entries: Vec<String> = std::fs::read_dir(&full_path)
        .map_err(|e| adk_core::AdkError::Tool(e.to_string()))?
        .filter_map(|e| e.ok().map(|e| e.file_name().to_string_lossy().to_string()))
        .collect();
    return Ok(json!({"type": "directory", "path": path, "entries": entries}));
}

let size = meta.len();
if size > max {
    return Ok(json!({"error": "File too large", "size": size, "max": max}));
}

match std::fs::read_to_string(&full_path) {
    Ok(content) => Ok(json!({
        "type": "text", "path": path, "size": size,
        "lines": content.lines().count(), "content": content
    })),
    Err(_) => {
        let bytes = std::fs::read(&full_path).map_err(|e| adk_core::AdkError::Tool(e.to_string()))?;
        Ok(json!({
            "type": "binary", "path": path, "size": size,
            "content": base64::Engine::encode(&base64::engine::general_purpose::STANDARD, &bytes)
        }))
    }
}`
                          }},
                          { name: 'Write File', icon: '💾', template: {
                            name: 'write_file',
                            description: 'Write file to session workspace (isolated per session)',
                            parameters: [
                              { name: 'path', param_type: 'string' as const, description: 'File path relative to session workspace', required: true },
                              { name: 'content', param_type: 'string' as const, description: 'Content to write', required: true },
                              { name: 'append', param_type: 'boolean' as const, description: 'Append instead of overwrite', required: false },
                            ],
                            code: `// Session-isolated workspace
let session_id = std::env::args().nth(1).unwrap_or_else(|| "default".to_string());
let workspace = std::path::PathBuf::from(format!("/tmp/adk-workspace/{}", session_id));

let full_path = workspace.join(path.trim_start_matches('/'));

// Create parent directories
if let Some(parent) = full_path.parent() {
    std::fs::create_dir_all(parent)
        .map_err(|e| adk_core::AdkError::Tool(e.to_string()))?;
}

if append {
    use std::io::Write;
    let mut file = std::fs::OpenOptions::new()
        .create(true).append(true).open(&full_path)
        .map_err(|e| adk_core::AdkError::Tool(e.to_string()))?;
    file.write_all(content.as_bytes())
        .map_err(|e| adk_core::AdkError::Tool(e.to_string()))?;
} else {
    std::fs::write(&full_path, content)
        .map_err(|e| adk_core::AdkError::Tool(e.to_string()))?;
}
Ok(json!({"status": "written", "path": path, "bytes": content.len(), "workspace": workspace.display().to_string()}))`
                          }},
                          { name: 'Run Command', icon: '⚡', template: {
                            name: 'run_command',
                            description: 'Execute a shell command',
                            parameters: [
                              { name: 'command', param_type: 'string' as const, description: 'Command to execute', required: true },
                            ],
                            code: `let output = std::process::Command::new("sh")
    .arg("-c")
    .arg(command)
    .output()
    .map_err(|e| adk_core::AdkError::Tool(e.to_string()))?;
let stdout = String::from_utf8_lossy(&output.stdout);
let stderr = String::from_utf8_lossy(&output.stderr);
Ok(json!({"stdout": stdout, "stderr": stderr, "success": output.status.success()}))`
                          }},
                          { name: 'JSON Transform', icon: '🔄', template: {
                            name: 'json_transform',
                            description: 'Transform JSON data using JSONPath',
                            parameters: [
                              { name: 'data', param_type: 'string' as const, description: 'JSON string to transform', required: true },
                              { name: 'path', param_type: 'string' as const, description: 'JSONPath expression', required: true },
                            ],
                            code: `let parsed: Value = serde_json::from_str(data)
    .map_err(|e| adk_core::AdkError::Tool(e.to_string()))?;
// Simple path extraction (for complex paths, use jsonpath crate)
let result = path.split('.').fold(Some(&parsed), |acc, key| {
    acc.and_then(|v| v.get(key))
});
Ok(json!({"result": result}))`
                          }},
                          { name: 'Database Query', icon: '🗄️', template: {
                            name: 'db_query',
                            description: 'Execute a database query',
                            parameters: [
                              { name: 'query', param_type: 'string' as const, description: 'SQL query to execute', required: true },
                              { name: 'connection_string', param_type: 'string' as const, description: 'Database connection string', required: true },
                            ],
                            code: `// Requires: sqlx or rusqlite crate
// let pool = SqlitePool::connect(connection_string).await?;
// let rows = sqlx::query(query).fetch_all(&pool).await?;
Ok(json!({"status": "executed", "query": query}))`
                          }},
                          { name: 'Webhook', icon: '🪝', template: {
                            name: 'send_webhook',
                            description: 'Send data to a webhook URL',
                            parameters: [
                              { name: 'webhook_url', param_type: 'string' as const, description: 'Webhook URL', required: true },
                              { name: 'payload', param_type: 'string' as const, description: 'JSON payload', required: true },
                            ],
                            code: `let client = reqwest::Client::new();
let resp = client.post(webhook_url)
    .header("Content-Type", "application/json")
    .body(payload.to_string())
    .send()
    .await
    .map_err(|e| adk_core::AdkError::Tool(e.to_string()))?;
Ok(json!({"status": resp.status().as_u16(), "sent": true}))`
                          }},
                          { name: 'Slack Message', icon: '💬', template: {
                            name: 'send_slack',
                            description: 'Send a message to Slack',
                            parameters: [
                              { name: 'webhook_url', param_type: 'string' as const, description: 'Slack webhook URL', required: true },
                              { name: 'message', param_type: 'string' as const, description: 'Message to send', required: true },
                              { name: 'channel', param_type: 'string' as const, description: 'Channel (optional)', required: false },
                            ],
                            code: `let payload = json!({"text": message, "channel": channel});
let client = reqwest::Client::new();
client.post(webhook_url)
    .json(&payload)
    .send()
    .await
    .map_err(|e| adk_core::AdkError::Tool(e.to_string()))?;
Ok(json!({"status": "sent", "message": message}))`
                          }},
                          { name: 'Calculator', icon: '🧮', template: {
                            name: 'calculate',
                            description: 'Perform mathematical calculations',
                            parameters: [
                              { name: 'operation', param_type: 'string' as const, description: 'add, subtract, multiply, divide', required: true },
                              { name: 'a', param_type: 'number' as const, description: 'First number', required: true },
                              { name: 'b', param_type: 'number' as const, description: 'Second number', required: true },
                            ],
                            code: `let result = match operation {
    "add" => a + b,
    "subtract" => a - b,
    "multiply" => a * b,
    "divide" => if b != 0.0 { a / b } else { return Ok(json!({"error": "Division by zero"})); },
    _ => return Ok(json!({"error": "Unknown operation"})),
};
Ok(json!({"result": result, "operation": operation}))`
                          }},
                        ].map(t => (
                          <button
                            key={t.name}
                            className="px-2 py-1.5 bg-gray-700 hover:bg-gray-600 rounded text-xs text-left flex items-center gap-1"
                            title="Click to apply, Shift+Click to add as new tool"
                            onClick={(e) => {
                              if (e.shiftKey && selectedNodeId) {
                                // Add as new function tool
                                addToolToAgent(selectedNodeId, 'function');
                                const agentTools = currentProject?.agents[selectedNodeId]?.tools || [];
                                const functionCount = agentTools.filter(t => t.startsWith('function')).length;
                                const newToolId = `${selectedNodeId}_function_${functionCount + 1}`;
                                setTimeout(() => {
                                  updateToolConfig(newToolId, { type: 'function', ...t.template });
                                  selectTool(newToolId);
                                }, 50);
                              } else {
                                updateToolConfig(selectedToolId, { ...fnConfig, ...t.template });
                              }
                            }}
                          >
                            <span>{t.icon}</span>
                            <span className="truncate">{t.name}</span>
                          </button>
                        ))}
                      </div>
                      <div className="text-xs text-gray-500 mt-1">Shift+Click to add as new tool</div>
                    </div>
                  </div>
                );
              })()}
              
              {actualToolType === 'browser' && (() => {
                const browserConfig = currentConfig as BrowserToolConfig;
                return (
                  <div className="space-y-3">
                    <div className="p-2 bg-yellow-900/50 border border-yellow-600 rounded text-xs">
                      <div className="font-semibold text-yellow-400 mb-1">⚠️ Requirements</div>
                      <ul className="list-disc list-inside text-yellow-200 space-y-1">
                        <li>Chrome or Chromium browser</li>
                        <li>ChromeDriver (matching Chrome version)</li>
                        <li>ChromeDriver running: <code className="bg-black/30 px-1 rounded">chromedriver --port=4444</code></li>
                      </ul>
                    </div>
                    <div className="flex items-center gap-2">
                      <input
                        type="checkbox"
                        id="headless"
                        checked={browserConfig.headless}
                        onChange={(e) => updateToolConfig(selectedToolId, { ...browserConfig, headless: e.target.checked })}
                      />
                      <label htmlFor="headless" className="text-sm">Headless Mode</label>
                    </div>
                    <div>
                      <label className="block text-sm text-gray-400 mb-1">Timeout (ms)</label>
                      <input
                        type="number"
                        className="w-full px-2 py-1 bg-studio-bg border border-gray-600 rounded text-sm"
                        value={browserConfig.timeout_ms}
                        onChange={(e) => updateToolConfig(selectedToolId, { ...browserConfig, timeout_ms: parseInt(e.target.value) || 30000 })}
                      />
                    </div>
                    <div className="text-xs text-gray-500 mt-2">
                      Tools: navigate, click, type, screenshot, get_text
                    </div>
                  </div>
                );
              })()}

              {actualToolType === 'exit_loop' && (
                <div className="space-y-3">
                  <div className="p-2 bg-blue-900/50 border border-blue-600 rounded text-xs">
                    <div className="font-semibold text-blue-400 mb-1">ℹ️ Exit Loop Tool</div>
                    <p className="text-blue-200">Allows the agent to exit a loop when the task is complete.</p>
                    <p className="text-blue-200 mt-2">Use with Loop Agent to let the LLM decide when to stop iterating.</p>
                  </div>
                  <div className="text-xs text-gray-500">No configuration needed.</div>
                </div>
              )}

              {actualToolType === 'google_search' && (
                <div className="space-y-3">
                  <div className="p-2 bg-blue-900/50 border border-blue-600 rounded text-xs">
                    <div className="font-semibold text-blue-400 mb-1">ℹ️ Google Search Tool</div>
                    <p className="text-blue-200">Enables web search via Google's Grounding API.</p>
                    <p className="text-blue-200 mt-2">Requires Gemini model with grounding support.</p>
                  </div>
                  <div className="text-xs text-gray-500">No configuration needed.</div>
                </div>
              )}

              {actualToolType === 'load_artifact' && (
                <div className="space-y-3">
                  <div className="p-2 bg-blue-900/50 border border-blue-600 rounded text-xs">
                    <div className="font-semibold text-blue-400 mb-1">ℹ️ Load Artifact Tool</div>
                    <p className="text-blue-200">Loads artifacts from the session store.</p>
                    <p className="text-blue-200 mt-2">Use to retrieve files, images, or data saved by other agents.</p>
                  </div>
                  <div className="text-xs text-gray-500">No configuration needed.</div>
                </div>
              )}
            </div>
          );
        })()}
      </div>

      {/* Test Console */}
      {showConsole && hasAgents && builtBinaryPath && (
        <div className="h-64">
          <TestConsole onFlowPhase={setFlowPhase} onActiveAgent={setActiveAgent} onIteration={setIteration} binaryPath={builtBinaryPath} />
        </div>
      )}
      {showConsole && hasAgents && !builtBinaryPath && (
        <div className="h-32 bg-studio-panel border-t border-gray-700 flex items-center justify-center text-gray-500">
          <div className="text-center">
            <div>Build the project first to test it</div>
            <button onClick={handleBuild} className="mt-2 px-4 py-1 bg-blue-600 hover:bg-blue-700 rounded text-white text-sm">
              Build Project
            </button>
          </div>
        </div>
      )}
      {showConsole && !hasAgents && (
        <div className="h-32 bg-studio-panel border-t border-gray-700 flex items-center justify-center text-gray-500">
          Drag "LLM Agent" onto the canvas to get started
        </div>
      )}

      {/* Compiled Code Modal */}
      {compiledCode && (
        <div className="fixed inset-0 bg-black/70 flex items-center justify-center z-50" onClick={() => setCompiledCode(null)}>
          <div className="bg-studio-panel rounded-lg w-4/5 h-4/5 flex flex-col" onClick={e => e.stopPropagation()}>
            <div className="flex justify-between items-center p-4 border-b border-gray-700">
              <h2 className="text-lg font-semibold">Generated Rust Code</h2>
              <button onClick={() => setCompiledCode(null)} className="text-gray-400 hover:text-white text-xl">×</button>
            </div>
            <div className="flex-1 overflow-auto p-4">
              {compiledCode.files.map(file => (
                <div key={file.path} className="mb-6">
                  <div className="flex justify-between items-center mb-2">
                    <h3 className="text-sm font-mono text-blue-400">{file.path}</h3>
                    <button 
                      onClick={() => navigator.clipboard.writeText(file.content)}
                      className="text-xs bg-gray-700 px-2 py-1 rounded hover:bg-gray-600"
                    >Copy</button>
                  </div>
                  <div className="border border-gray-700 rounded overflow-hidden">
                    <Editor
                      height={Math.min(600, file.content.split('\n').length * 19 + 20)}
                      language={file.path.endsWith('.toml') ? 'toml' : 'rust'}
                      value={file.content}
                      theme="vs-dark"
                      options={{
                        readOnly: true,
                        minimap: { enabled: false },
                        scrollBeyondLastLine: false,
                        fontSize: 12,
                        lineNumbers: 'on',
                        folding: true,
                      }}
                    />
                  </div>
                </div>
              ))}
            </div>
          </div>
        </div>
      )}

      {/* Build Output Modal */}
      {buildOutput && (
        <div className="fixed inset-0 bg-black/70 flex items-center justify-center z-50" onClick={() => setBuildOutput(null)}>
          <div className="bg-studio-panel rounded-lg w-3/5 max-h-4/5 flex flex-col" onClick={e => e.stopPropagation()}>
            <div className="flex justify-between items-center p-4 border-b border-gray-700">
              <h2 className={`text-lg font-semibold ${building ? 'text-blue-400' : buildOutput.success ? 'text-green-400' : 'text-red-400'}`}>
                {building ? '⏳ Building...' : buildOutput.success ? '✓ Build Successful' : '✗ Build Failed'}
              </h2>
              <button onClick={() => setBuildOutput(null)} className="text-gray-400 hover:text-white text-xl">×</button>
            </div>
            <div className="flex-1 overflow-auto p-4">
              {buildOutput.path && (
                <div className="mb-4 p-3 bg-green-900/30 rounded">
                  <div className="text-sm text-gray-400">Binary path:</div>
                  <code className="text-green-400 text-sm">{buildOutput.path}</code>
                </div>
              )}
              <pre 
                ref={el => { if (el && building) el.scrollTop = el.scrollHeight; }}
                className="bg-gray-900 p-4 rounded text-xs overflow-auto whitespace-pre max-h-96"
              >{buildOutput.output}</pre>
            </div>
          </div>
        </div>
      )}

      {/* Code Editor Modal */}
      {showCodeEditor && selectedToolId && (() => {
        const toolConfig = currentProject?.tool_configs?.[selectedToolId];
        if (!toolConfig || toolConfig.type !== 'function') return null;
        const fnConfig = toolConfig as FunctionToolConfig;
        return (
          <div className="fixed inset-0 bg-black/80 flex items-center justify-center z-50" onClick={() => setShowCodeEditor(false)}>
            <div className="bg-studio-panel rounded-lg w-11/12 h-5/6 flex flex-col" onClick={e => e.stopPropagation()}>
              <div className="flex justify-between items-center p-4 border-b border-gray-700">
                <h2 className="text-lg font-semibold text-blue-400">
                  {fnConfig.name || 'function'}_fn
                </h2>
                <button onClick={() => setShowCodeEditor(false)} className="text-gray-400 hover:text-white text-xl">×</button>
              </div>
              <div className="flex-1 overflow-hidden">
                <Editor
                  height="100%"
                  defaultLanguage="rust"
                  theme="vs-dark"
                  value={generateFunctionTemplate(fnConfig)}
                  onChange={(value) => {
                    if (value) {
                      const code = extractUserCode(value, fnConfig);
                      updateToolConfig(selectedToolId, { ...fnConfig, code });
                    }
                  }}
                  options={{
                    minimap: { enabled: false },
                    fontSize: 14,
                    lineNumbers: 'on',
                    scrollBeyondLastLine: false,
                    automaticLayout: true,
                    tabSize: 4,
                  }}
                />
              </div>
              <div className="p-4 border-t border-gray-700 flex justify-end gap-2">
                <button 
                  onClick={() => setShowCodeEditor(false)}
                  className="px-4 py-2 bg-blue-600 hover:bg-blue-500 rounded text-sm"
                >
                  Done
                </button>
              </div>
            </div>
          </div>
        );
      })()}
    </div>
  );
}

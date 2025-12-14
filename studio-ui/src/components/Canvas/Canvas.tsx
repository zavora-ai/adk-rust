import { useCallback, useEffect, useState } from 'react';
import {
  ReactFlow,
  Background,
  Controls,
  Node,
  Edge,
  useNodesState,
  useEdgesState,
  addEdge,
  Connection,
} from '@xyflow/react';
import '@xyflow/react/dist/style.css';
import { useStore } from '../../store';
import { TestConsole } from '../Console/TestConsole';

export function Canvas() {
  const { currentProject, closeProject, saveProject, selectNode, selectedNodeId, updateAgent, addAgent, addEdge: addProjectEdge } = useStore();
  const [showConsole, setShowConsole] = useState(true);

  const [nodes, setNodes, onNodesChange] = useNodesState<Node>([]);
  const [edges, setEdges, onEdgesChange] = useEdgesState<Edge>([]);

  // Update canvas whenever project changes
  useEffect(() => {
    if (!currentProject) return;
    
    const newNodes: Node[] = [
      { id: 'START', position: { x: 50, y: 200 }, data: { label: 'START' }, type: 'input' },
      { id: 'END', position: { x: 600, y: 200 }, data: { label: 'END' }, type: 'output' },
    ];
    Object.entries(currentProject.agents).forEach(([id, agent]) => {
      newNodes.push({
        id,
        position: { x: agent.position.x, y: agent.position.y },
        data: { label: id },
        style: { background: '#16213e', border: '1px solid #e94560', borderRadius: 8, padding: 10, color: '#fff' },
      });
    });
    setNodes(newNodes);

    const newEdges: Edge[] = currentProject.workflow.edges.map((e, i) => ({
      id: `e${i}`,
      source: e.from,
      target: e.to,
      animated: true,
      style: { stroke: '#e94560' },
    }));
    setEdges(newEdges);
  }, [currentProject, setNodes, setEdges]);

  const handleAddAgent = () => {
    if (!currentProject) return;
    const id = `agent_${Object.keys(currentProject.agents).length + 1}`;
    addAgent(id, {
      type: 'llm',
      model: 'gemini-2.0-flash',
      instruction: 'You are a helpful assistant.',
      tools: [],
      sub_agents: [],
      position: { x: 300, y: 200 },
    });
    addProjectEdge('START', id);
    addProjectEdge(id, 'END');
    selectNode(id);
  };

  const onConnect = useCallback((params: Connection) => setEdges((eds) => addEdge(params, eds)), [setEdges]);

  const onNodeClick = useCallback((_: React.MouseEvent, node: Node) => {
    if (node.id !== 'START' && node.id !== 'END') {
      selectNode(node.id);
    } else {
      selectNode(null);
    }
  }, [selectNode]);

  const onPaneClick = useCallback(() => {
    selectNode(null);
  }, [selectNode]);

  if (!currentProject) return null;

  const selectedAgent = selectedNodeId ? currentProject.agents[selectedNodeId] : null;
  const hasAgents = Object.keys(currentProject.agents).length > 0;

  return (
    <div className="flex flex-col h-full">
      <div className="flex flex-1 overflow-hidden">
        {/* Palette */}
        <div className="w-48 bg-studio-panel border-r border-gray-700 p-4 flex flex-col">
          <h3 className="font-semibold mb-4">Components</h3>
          <div className="space-y-2 flex-1">
            <div 
              onClick={handleAddAgent}
              className="p-2 bg-studio-accent rounded cursor-pointer hover:bg-studio-highlight transition-colors text-sm"
            >
              + LLM Agent
            </div>
            <div className="p-2 bg-studio-accent rounded opacity-50 cursor-not-allowed text-sm">Tool Agent</div>
            <div className="p-2 bg-studio-accent rounded opacity-50 cursor-not-allowed text-sm">Condition</div>
          </div>
          <div className="space-y-2">
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
            onConnect={onConnect}
            onNodeClick={onNodeClick}
            onPaneClick={onPaneClick}
            fitView
          >
            <Background color="#333" gap={20} />
            <Controls />
          </ReactFlow>
        </div>

        {/* Properties */}
        {selectedAgent && (
          <div className="w-64 bg-studio-panel border-l border-gray-700 p-4">
            <div className="flex justify-between items-center mb-4">
              <h3 className="font-semibold">{selectedNodeId}</h3>
              <div className="flex gap-2">
                <button onClick={saveProject} className="px-2 py-1 bg-studio-highlight rounded text-xs">Save</button>
                <button onClick={() => selectNode(null)} className="px-2 py-1 bg-gray-600 rounded text-xs">Close</button>
              </div>
            </div>
            <label className="block text-sm text-gray-400 mb-1">Model</label>
            <input
              className="w-full px-2 py-1 bg-studio-bg border border-gray-600 rounded text-sm mb-3"
              value={selectedAgent.model || ''}
              onChange={(e) => updateAgent(selectedNodeId!, { model: e.target.value })}
            />
            <label className="block text-sm text-gray-400 mb-1">Instruction</label>
            <textarea
              className="w-full px-2 py-1 bg-studio-bg border border-gray-600 rounded text-sm h-32"
              value={selectedAgent.instruction}
              onChange={(e) => updateAgent(selectedNodeId!, { instruction: e.target.value })}
            />
          </div>
        )}
      </div>

      {/* Test Console */}
      {showConsole && hasAgents && (
        <div className="h-64">
          <TestConsole />
        </div>
      )}
      {showConsole && !hasAgents && (
        <div className="h-32 bg-studio-panel border-t border-gray-700 flex items-center justify-center text-gray-500">
          Click "+ LLM Agent" to add an agent
        </div>
      )}
    </div>
  );
}

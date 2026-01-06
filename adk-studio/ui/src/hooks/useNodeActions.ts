import { useCallback } from 'react';
import { useStore } from '../store';
import type { AgentSchema } from '../types/project';

export function useNodeActions() {
  const { currentProject, addAgent, updateAgent: storeUpdateAgent, removeAgent, addEdge, removeEdge } = useStore();

  const createAgent = useCallback((type: string) => {
    if (!currentProject) return null;
    const count = Object.keys(currentProject.agents).length;
    const prefix = { sequential: 'seq', loop: 'loop', parallel: 'par', router: 'router' }[type] || 'agent';
    const id = `${prefix}_${count + 1}`;

    const defaults: Record<string, Partial<AgentSchema>> = {
      llm: { type: 'llm', model: 'gemini-2.0-flash', instruction: 'You are a helpful assistant.', tools: [], sub_agents: [] },
      sequential: { type: 'sequential', instruction: '', tools: [], sub_agents: [] },
      loop: { type: 'loop', instruction: '', tools: [], sub_agents: [], max_iterations: 3 },
      parallel: { type: 'parallel', instruction: '', tools: [], sub_agents: [] },
      router: { type: 'router', model: 'gemini-2.0-flash', instruction: 'Route based on intent.', tools: [], sub_agents: [], routes: [{ condition: 'default', target: 'END' }] },
    };

    addAgent(id, { ...defaults[type], position: { x: 50, y: 150 + count * 120 } } as AgentSchema);

    // Wire into workflow
    const edgeToEnd = currentProject.workflow.edges.find(e => e.to === 'END');
    if (edgeToEnd) { removeEdge(edgeToEnd.from, 'END'); addEdge(edgeToEnd.from, id); }
    else addEdge('START', id);
    addEdge(id, 'END');

    return id;
  }, [currentProject, addAgent, addEdge, removeEdge]);

  const duplicateAgent = useCallback((nodeId: string) => {
    if (!currentProject) return null;
    const agent = currentProject.agents[nodeId];
    if (!agent) return null;
    const newId = `${nodeId}_copy`;
    addAgent(newId, { ...agent, position: { x: (agent.position?.x || 50) + 50, y: (agent.position?.y || 50) + 50 } });
    return newId;
  }, [currentProject, addAgent]);

  const deleteAgent = useCallback((nodeId: string) => {
    if (nodeId === 'START' || nodeId === 'END') return;
    removeAgent(nodeId);
  }, [removeAgent]);

  return { createAgent, duplicateAgent, deleteAgent, updateAgent: storeUpdateAgent };
}

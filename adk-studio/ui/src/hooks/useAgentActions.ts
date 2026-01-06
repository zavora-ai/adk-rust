import { useCallback } from 'react';
import { useStore } from '../store';
import type { AgentSchema } from '../types/project';

export function useAgentActions() {
  const { currentProject, addAgent, removeAgent, addEdge, removeEdge, selectNode } = useStore();

  const createAgent = useCallback((agentType: string = 'llm') => {
    if (!currentProject) return;
    const count = Object.keys(currentProject.agents).length;
    const prefix = { sequential: 'seq', loop: 'loop', parallel: 'par', router: 'router' }[agentType] || 'agent';
    const id = `${prefix}_${count + 1}`;

    if (['sequential', 'loop', 'parallel'].includes(agentType)) {
      const sub1 = `${id}_agent_1`, sub2 = `${id}_agent_2`, isLoop = agentType === 'loop';
      addAgent(sub1, { type: 'llm', model: 'gemini-2.0-flash', instruction: isLoop ? 'Process and refine.' : 'Agent 1.', tools: [], sub_agents: [], position: { x: 0, y: 0 } });
      addAgent(sub2, { type: 'llm', model: 'gemini-2.0-flash', instruction: isLoop ? 'Review. Call exit_loop when done.' : 'Agent 2.', tools: isLoop ? ['exit_loop'] : [], sub_agents: [], position: { x: 0, y: 0 } });
      addAgent(id, { type: agentType as AgentSchema['type'], instruction: '', tools: [], sub_agents: [sub1, sub2], position: { x: 50, y: 150 + count * 180 }, max_iterations: isLoop ? 3 : undefined });
    } else if (agentType === 'router') {
      addAgent(id, { type: 'router', model: 'gemini-2.0-flash', instruction: 'Route based on intent.', tools: [], sub_agents: [], position: { x: 50, y: 150 + count * 120 }, routes: [{ condition: 'default', target: 'END' }] });
    } else {
      addAgent(id, { type: 'llm', model: 'gemini-2.0-flash', instruction: 'You are a helpful assistant.', tools: [], sub_agents: [], position: { x: 50, y: 150 + count * 120 } });
    }

    const edgeToEnd = currentProject.workflow.edges.find(e => e.to === 'END');
    if (edgeToEnd) { removeEdge(edgeToEnd.from, 'END'); addEdge(edgeToEnd.from, id); }
    else addEdge('START', id);
    addEdge(id, 'END');
    selectNode(id);
  }, [currentProject, addAgent, addEdge, removeEdge, selectNode]);

  const duplicateAgent = useCallback((nodeId: string) => {
    if (!currentProject) return null;
    const agent = currentProject.agents[nodeId];
    if (!agent) return null;
    const newId = `${nodeId}_copy`;
    addAgent(newId, { ...agent, position: { x: (agent.position?.x || 50) + 50, y: (agent.position?.y || 50) + 50 } });
    return newId;
  }, [currentProject, addAgent]);

  return { createAgent, duplicateAgent, removeAgent };
}

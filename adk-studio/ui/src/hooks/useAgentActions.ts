import { useCallback } from 'react';
import { useStore } from '../store';
import type { AgentSchema } from '../types/project';

export function useAgentActions() {
  const addAgent = useStore(s => s.addAgent);
  const removeAgent = useStore(s => s.removeAgent);
  const addEdge = useStore(s => s.addEdge);
  const removeEdge = useStore(s => s.removeEdge);
  const selectNode = useStore(s => s.selectNode);

  /**
   * Create a new agent and wire it into the workflow.
   * @param agentType - Type of agent to create
   * @param skipWiring - If true, skip automatic edge wiring (caller will handle it)
   * @returns The new agent's ID, or undefined if creation failed
   */
  const createAgent = useCallback((agentType: string = 'llm', skipWiring?: boolean): string | undefined => {
    // Read current project from store directly to avoid stale closures
    const currentProject = useStore.getState().currentProject;
    if (!currentProject) return;
    
    const prefix = { sequential: 'seq', loop: 'loop', parallel: 'par', router: 'router' }[agentType] || 'agent';
    
    // Find the next available ID by checking existing agent IDs
    // This handles gaps from deleted agents (e.g., if agent_1 is deleted, don't reuse it)
    const existingIds = Object.keys(currentProject.agents);
    const prefixPattern = new RegExp(`^${prefix}_(\\d+)$`);
    let maxNum = 0;
    for (const existingId of existingIds) {
      const match = existingId.match(prefixPattern);
      if (match) {
        maxNum = Math.max(maxNum, parseInt(match[1], 10));
      }
    }
    const id = `${prefix}_${maxNum + 1}`;
    
    // Use existing agent count for position calculation
    // Default horizontal layout: agents positioned left-to-right
    const agentCount = existingIds.length;
    const baseX = 250;  // Start after START node (which is at x: 100)
    const baseY = 100;  // Same vertical level as START
    const horizontalSpacing = 200;  // Space between agents horizontally
    const containerSpacing = 250;   // Wider spacing for container agents

    if (['sequential', 'loop', 'parallel'].includes(agentType)) {
      const sub1 = `${id}_agent_1`, sub2 = `${id}_agent_2`, isLoop = agentType === 'loop';
      addAgent(sub1, { type: 'llm', model: 'gemini-2.0-flash', instruction: isLoop ? 'Process and refine.' : 'Agent 1.', tools: ['google_search'], sub_agents: [], position: { x: 0, y: 0 } });
      addAgent(sub2, { type: 'llm', model: 'gemini-2.0-flash', instruction: isLoop ? 'Review. Call exit_loop when done.' : 'Agent 2.', tools: isLoop ? ['exit_loop', 'google_search'] : ['google_search'], sub_agents: [], position: { x: 0, y: 0 } });
      addAgent(id, { type: agentType as AgentSchema['type'], instruction: '', tools: [], sub_agents: [sub1, sub2], position: { x: baseX + agentCount * containerSpacing, y: baseY }, max_iterations: isLoop ? 3 : undefined });
    } else if (agentType === 'router') {
      addAgent(id, { type: 'router', model: 'gemini-2.0-flash', instruction: 'Route based on intent.', tools: ['google_search'], sub_agents: [], position: { x: baseX + agentCount * horizontalSpacing, y: baseY }, routes: [{ condition: 'default', target: 'END' }] });
    } else {
      addAgent(id, { type: 'llm', model: 'gemini-2.0-flash', instruction: 'You are a helpful assistant.', tools: ['google_search'], sub_agents: [], position: { x: baseX + agentCount * horizontalSpacing, y: baseY } });
    }

    // Re-read current project to get the latest edges after adding agents
    const updatedProject = useStore.getState().currentProject;
    if (!updatedProject) return id;
    
    if (!skipWiring) {
      const edgeToEnd = updatedProject.workflow.edges.find(e => e.to === 'END');
      if (edgeToEnd) { removeEdge(edgeToEnd.from, 'END'); addEdge(edgeToEnd.from, id); }
      else addEdge('START', id);
      addEdge(id, 'END');
    }
    selectNode(id);
    return id;
  }, [addAgent, addEdge, removeEdge, selectNode]);

  const duplicateAgent = useCallback((nodeId: string) => {
    // Read current project from store directly to avoid stale closures
    const currentProject = useStore.getState().currentProject;
    if (!currentProject) return null;
    const agent = currentProject.agents[nodeId];
    if (!agent) return null;
    const newId = `${nodeId}_copy`;
    addAgent(newId, { ...agent, position: { x: (agent.position?.x || 50) + 50, y: (agent.position?.y || 50) + 50 } });
    return newId;
  }, [addAgent]);

  return { createAgent, duplicateAgent, removeAgent };
}

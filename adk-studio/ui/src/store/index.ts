import { create } from 'zustand';
import type { Project, ProjectMeta, AgentSchema, ToolConfig } from '../types/project';
import type { LayoutDirection } from '../types/layout';
import { api } from '../api/client';

interface StudioState {
  // Project list
  projects: ProjectMeta[];
  loadingProjects: boolean;
  
  // Current project
  currentProject: Project | null;
  selectedNodeId: string | null;
  selectedToolId: string | null;
  layoutDirection: LayoutDirection;
  
  // Actions
  fetchProjects: () => Promise<void>;
  createProject: (name: string, description?: string) => Promise<Project>;
  openProject: (id: string) => Promise<void>;
  saveProject: () => Promise<void>;
  closeProject: () => void;
  deleteProject: (id: string) => Promise<void>;
  
  // Canvas actions
  selectNode: (id: string | null) => void;
  updateAgent: (id: string, updates: Partial<AgentSchema>) => void;
  renameAgent: (oldId: string, newId: string) => void;
  addAgent: (id: string, agent: AgentSchema) => void;
  removeAgent: (id: string) => void;
  addEdge: (from: string, to: string) => void;
  removeEdge: (from: string, to: string) => void;
  addToolToAgent: (agentId: string, toolType: string) => void;
  removeToolFromAgent: (agentId: string, toolType: string) => void;
  addSubAgentToContainer: (containerId: string) => void;
  
  // Tool config actions
  selectTool: (toolId: string | null) => void;
  updateToolConfig: (toolId: string, config: ToolConfig) => void;
  
  // Layout actions
  setLayoutDirection: (dir: LayoutDirection) => void;
}

export const useStore = create<StudioState>((set, get) => ({
  projects: [],
  loadingProjects: false,
  currentProject: null,
  selectedNodeId: null,
  selectedToolId: null,
  layoutDirection: 'TB',

  fetchProjects: async () => {
    set({ loadingProjects: true });
    try {
      const projects = await api.projects.list();
      set({ projects });
    } finally {
      set({ loadingProjects: false });
    }
  },

  createProject: async (name, description) => {
    const project = await api.projects.create(name, description);
    set((s) => ({ projects: [{ id: project.id, name, description: description || '', updated_at: project.updated_at }, ...s.projects] }));
    return project;
  },

  openProject: async (id) => {
    const project = await api.projects.get(id);
    set({ currentProject: project, selectedNodeId: null });
  },

  saveProject: async () => {
    const { currentProject } = get();
    if (!currentProject) return;
    await api.projects.update(currentProject.id, currentProject);
  },

  closeProject: () => set({ currentProject: null, selectedNodeId: null }),

  deleteProject: async (id) => {
    await api.projects.delete(id);
    set((s) => ({ projects: s.projects.filter((p) => p.id !== id) }));
  },

  selectNode: (id) => set({ selectedNodeId: id }),

  updateAgent: (id, updates) =>
    set((s) => {
      if (!s.currentProject) return s;
      return {
        currentProject: {
          ...s.currentProject,
          agents: {
            ...s.currentProject.agents,
            [id]: { ...s.currentProject.agents[id], ...updates },
          },
        },
      };
    }),

  renameAgent: (oldId, newId) => {
    if (oldId === newId) return;
    set((s) => {
      if (!s.currentProject || !s.currentProject.agents[oldId]) return s;
      
      // Clone agents, add new key, remove old
      const agents = { ...s.currentProject.agents };
      agents[newId] = agents[oldId];
      delete agents[oldId];
      
      // Update sub_agents references in containers
      Object.keys(agents).forEach(id => {
        if (agents[id].sub_agents?.includes(oldId)) {
          agents[id] = { ...agents[id], sub_agents: agents[id].sub_agents.map(s => s === oldId ? newId : s) };
        }
      });
      
      // Update edges
      const edges = s.currentProject.workflow.edges.map(e => ({
        ...e,
        from: e.from === oldId ? newId : e.from,
        to: e.to === oldId ? newId : e.to,
      }));
      
      // Update tool configs
      const toolConfigs = { ...s.currentProject.tool_configs };
      Object.keys(toolConfigs).forEach(key => {
        if (key.startsWith(`${oldId}_`)) {
          const newKey = key.replace(`${oldId}_`, `${newId}_`);
          toolConfigs[newKey] = toolConfigs[key];
          delete toolConfigs[key];
        }
      });
      
      return {
        currentProject: { ...s.currentProject, agents, tool_configs: toolConfigs, workflow: { ...s.currentProject.workflow, edges } },
        selectedNodeId: s.selectedNodeId === oldId ? newId : s.selectedNodeId,
      };
    });
    get().saveProject();
  },

  addAgent: (id, agent) => {
    set((s) => {
      if (!s.currentProject) return s;
      return {
        currentProject: {
          ...s.currentProject,
          agents: { ...s.currentProject.agents, [id]: agent },
        },
      };
    });
    // Auto-save after state update
    setTimeout(() => get().saveProject(), 0);
  },

  removeAgent: (id) => {
    set((s) => {
      if (!s.currentProject) return s;
      const agent = s.currentProject.agents[id];
      
      // Collect all agents to remove (including sub-agents for containers)
      const agentsToRemove = [id];
      if (agent?.sub_agents) {
        agentsToRemove.push(...agent.sub_agents);
      }
      
      // Remove all agents
      const agents = { ...s.currentProject.agents };
      agentsToRemove.forEach(agentId => delete agents[agentId]);
      
      // Remove tool configs for all removed agents
      const toolConfigs = { ...s.currentProject.tool_configs };
      Object.keys(toolConfigs).forEach(key => {
        if (agentsToRemove.some(agentId => key.startsWith(`${agentId}_`))) {
          delete toolConfigs[key];
        }
      });
      
      return {
        currentProject: {
          ...s.currentProject,
          agents,
          tool_configs: toolConfigs,
          workflow: {
            ...s.currentProject.workflow,
            edges: s.currentProject.workflow.edges.filter((e) => 
              !agentsToRemove.includes(e.from) && !agentsToRemove.includes(e.to)
            ),
          },
        },
      };
    });
    setTimeout(() => get().saveProject(), 0);
  },

  addEdge: (from, to) => {
    set((s) => {
      if (!s.currentProject) return s;
      return {
        currentProject: {
          ...s.currentProject,
          workflow: {
            ...s.currentProject.workflow,
            edges: [...s.currentProject.workflow.edges, { from, to }],
          },
        },
      };
    });
    setTimeout(() => get().saveProject(), 0);
  },

  removeEdge: (from, to) => {
    set((s) => {
      if (!s.currentProject) return s;
      return {
        currentProject: {
          ...s.currentProject,
          workflow: {
            ...s.currentProject.workflow,
            edges: s.currentProject.workflow.edges.filter((e) => !(e.from === from && e.to === to)),
          },
        },
      };
    });
    setTimeout(() => get().saveProject(), 0);
  },

  addToolToAgent: (agentId, toolType) => {
    set((s) => {
      if (!s.currentProject) return s;
      const agent = s.currentProject.agents[agentId];
      if (!agent) return s;
      
      // For function and mcp tools, generate unique ID to allow multiple
      let toolId = toolType;
      if (toolType === 'function' || toolType === 'mcp') {
        const existing = agent.tools.filter(t => t.startsWith(toolType));
        toolId = `${toolType}_${existing.length + 1}`;
      } else if (agent.tools.includes(toolType)) {
        return s; // Other tools can only be added once
      }
      
      return {
        currentProject: {
          ...s.currentProject,
          agents: {
            ...s.currentProject.agents,
            [agentId]: { ...agent, tools: [...agent.tools, toolId] },
          },
        },
      };
    });
    setTimeout(() => get().saveProject(), 0);
  },

  removeToolFromAgent: (agentId, toolType) => {
    set((s) => {
      if (!s.currentProject) return s;
      const agent = s.currentProject.agents[agentId];
      if (!agent) return s;
      const toolConfigId = `${agentId}_${toolType}`;
      const { [toolConfigId]: _, ...remainingConfigs } = s.currentProject.tool_configs;
      return {
        currentProject: {
          ...s.currentProject,
          agents: {
            ...s.currentProject.agents,
            [agentId]: { ...agent, tools: agent.tools.filter(t => t !== toolType) },
          },
          tool_configs: remainingConfigs,
        },
        selectedToolId: s.selectedToolId === toolConfigId ? null : s.selectedToolId,
      };
    });
    setTimeout(() => get().saveProject(), 0);
  },

  addSubAgentToContainer: (containerId) => {
    const { currentProject, addAgent, updateAgent, saveProject } = get();
    if (!currentProject) return;
    const container = currentProject.agents[containerId];
    if (!container) return;
    const subCount = container.sub_agents.length + 1;
    const newId = `${containerId}_agent_${subCount}`;
    addAgent(newId, {
      type: 'llm',
      model: 'gemini-2.0-flash',
      instruction: `You are agent ${subCount}.`,
      tools: [],
      sub_agents: [],
      position: { x: 0, y: 0 },
    });
    updateAgent(containerId, { sub_agents: [...container.sub_agents, newId] });
    saveProject();
  },

  selectTool: (toolId) => set({ selectedToolId: toolId }),

  updateToolConfig: (toolId, config) => {
    set((s) => {
      if (!s.currentProject) return s;
      return {
        currentProject: {
          ...s.currentProject,
          tool_configs: { ...s.currentProject.tool_configs, [toolId]: config },
        },
      };
    });
    setTimeout(() => get().saveProject(), 0);
  },

  setLayoutDirection: (dir) => set({ layoutDirection: dir }),
}));

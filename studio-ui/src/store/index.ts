import { create } from 'zustand';
import type { Project, ProjectMeta, AgentSchema } from '../types/project';
import { api } from '../api/client';

interface StudioState {
  // Project list
  projects: ProjectMeta[];
  loadingProjects: boolean;
  
  // Current project
  currentProject: Project | null;
  selectedNodeId: string | null;
  
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
  addAgent: (id: string, agent: AgentSchema) => void;
  removeAgent: (id: string) => void;
  addEdge: (from: string, to: string) => void;
  removeEdge: (from: string, to: string) => void;
}

export const useStore = create<StudioState>((set, get) => ({
  projects: [],
  loadingProjects: false,
  currentProject: null,
  selectedNodeId: null,

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

  addAgent: (id, agent) =>
    set((s) => {
      if (!s.currentProject) return s;
      return {
        currentProject: {
          ...s.currentProject,
          agents: { ...s.currentProject.agents, [id]: agent },
        },
      };
    }),

  removeAgent: (id) =>
    set((s) => {
      if (!s.currentProject) return s;
      const { [id]: _, ...agents } = s.currentProject.agents;
      return {
        currentProject: {
          ...s.currentProject,
          agents,
          workflow: {
            ...s.currentProject.workflow,
            edges: s.currentProject.workflow.edges.filter((e) => e.from !== id && e.to !== id),
          },
        },
      };
    }),

  addEdge: (from, to) =>
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
    }),

  removeEdge: (from, to) =>
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
    }),
}));

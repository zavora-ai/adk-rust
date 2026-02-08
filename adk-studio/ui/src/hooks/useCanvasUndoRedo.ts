import { useCallback, useRef, useEffect } from 'react';
import { useStore } from '../store';
import { useUndoRedo, useUndoRedoStore } from './useUndoRedo';
import type { Edge as ProjectEdge } from '../types/project';
import type { AutobuildTriggerType } from './useBuild';

/**
 * Hook that manages undo/redo integration for the Canvas.
 * Wraps createAgent, removeAgent, and removeActionNode with undo recording
 * and handles project change tracking + initial layout application.
 *
 * @see Requirements 11.5, 11.6: Undo/Redo support
 * @see Requirements 2.5, 2.7: Canvas delegates handlers
 */
export function useCanvasUndoRedo(deps: {
  createAgent: (agentType?: string, skipWiring?: boolean) => string | undefined;
  removeAgent: (nodeId: string) => void;
  removeActionNode: (nodeId: string) => void;
  applyLayout: () => void;
  invalidateBuild: (reason?: AutobuildTriggerType) => void;
  currentProjectId: string | undefined;
  nodeCount: number;
}) {
  const {
    createAgent,
    removeAgent,
    removeActionNode,
    applyLayout,
    invalidateBuild,
    currentProjectId,
    nodeCount,
  } = deps;

  const {
    addAgent,
    addEdge: addProjectEdge,
    removeEdge: removeProjectEdge,
  } = useStore();

  const { clearHistory: clearUndoHistory } = useUndoRedoStore();

  // Helper to get edges connected to a node
  const getEdgesForNode = useCallback((nodeId: string) => {
    const project = useStore.getState().currentProject;
    if (!project) return [];
    return project.workflow.edges.filter(
      (e) => e.from === nodeId || e.to === nodeId
    );
  }, []);

  // Helper to get all edges
  const getAllEdges = useCallback(() => {
    return useStore.getState().currentProject?.workflow.edges || [];
  }, []);

  // Helper to get agent by ID
  const getAgent = useCallback((nodeId: string) => {
    return useStore.getState().currentProject?.agents[nodeId];
  }, []);

  // Helper to set all edges (for undo/redo)
  const setEdgesCallback = useCallback((edges: ProjectEdge[]) => {
    useStore.getState().setEdges(edges);
  }, []);

  // Undo/Redo hook with handlers
  const undoRedo = useUndoRedo({
    onAddNode: (nodeId, agent) => {
      addAgent(nodeId, agent);
    },
    onRemoveNode: (nodeId) => {
      useStore.getState().removeAgent(nodeId);
    },
    onAddEdge: addProjectEdge,
    onRemoveEdge: removeProjectEdge,
    onSetEdges: setEdgesCallback,
    getAgent,
    getEdgesForNode,
    getAllEdges,
  });

  // Wrapped createAgent that records for undo
  const createAgentWithUndo = useCallback((agentType?: string, skipWiring?: boolean): string | undefined => {
    const edgesBefore = [...(useStore.getState().currentProject?.workflow.edges || [])];

    const newId = createAgent(agentType, skipWiring);

    setTimeout(() => {
      const state = useStore.getState();
      const project = state.currentProject;
      if (!project) return;

      const newEdges = project.workflow.edges.filter(
        (e) => !edgesBefore.some((eb) => eb.from === e.from && eb.to === e.to)
      );

      const newAgentId = newEdges.find((e) => e.to === 'END')?.from
        || (newId && project.agents[newId] ? newId : undefined);
      if (newAgentId && project.agents[newAgentId]) {
        undoRedo.recordAddNode(newAgentId, project.agents[newAgentId], newEdges, edgesBefore);
      }
    }, 0);
    return newId;
  }, [createAgent, undoRedo]);

  // Wrapped removeAgent that records for undo and applies layout
  const removeAgentWithUndo = useCallback((nodeId: string) => {
    if (nodeId === 'START' || nodeId === 'END') return;

    const edgesBefore = [...(useStore.getState().currentProject?.workflow.edges || [])];
    undoRedo.recordDeleteNode(nodeId, edgesBefore);

    removeAgent(nodeId);

    invalidateBuild('onAgentDelete');
    setTimeout(() => applyLayout(), 100);
  }, [removeAgent, undoRedo, invalidateBuild, applyLayout]);

  // Wrapped removeActionNode that also applies layout after deletion
  const removeActionNodeWithLayout = useCallback((nodeId: string) => {
    removeActionNode(nodeId);
    invalidateBuild('onAgentDelete');
    setTimeout(() => applyLayout(), 100);
  }, [removeActionNode, invalidateBuild, applyLayout]);

  // Clear undo history when project changes
  const prevProjectIdRef = useRef<string | null>(null);
  const hasAppliedInitialLayout = useRef<string | null>(null);

  if (currentProjectId !== prevProjectIdRef.current) {
    prevProjectIdRef.current = currentProjectId || null;
    hasAppliedInitialLayout.current = null;
    clearUndoHistory();
  }

  // Apply layout when a new project is opened (after nodes are rendered)
  useEffect(() => {
    if (!currentProjectId) return;
    if (nodeCount === 0) return;

    if (hasAppliedInitialLayout.current === currentProjectId) return;

    const timer1 = setTimeout(() => {
      applyLayout();
    }, 100);

    const timer2 = setTimeout(() => {
      applyLayout();
      hasAppliedInitialLayout.current = currentProjectId;
    }, 300);

    return () => {
      clearTimeout(timer1);
      clearTimeout(timer2);
    };
  }, [currentProjectId, nodeCount, applyLayout]);

  return {
    undoRedo,
    createAgentWithUndo,
    removeAgentWithUndo,
    removeActionNodeWithLayout,
  };
}

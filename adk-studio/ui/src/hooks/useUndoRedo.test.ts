/**
 * Property-based tests for Undo/Redo MVP
 * 
 * **Feature: adk-studio-v2, Property 27: Undo/Redo Round-Trip** (CRITICAL)
 * *For any* sequence of add/delete node actions, undoing and then redoing
 * SHALL restore the exact same state.
 * 
 * **Validates: Requirements 11.5, 11.6**
 */

import { describe, it, expect, beforeEach } from 'vitest';
import * as fc from 'fast-check';
import { useUndoRedoStore, MAX_HISTORY_SIZE, type UndoableAction } from './useUndoRedo';
import type { AgentSchema, Edge } from '../types/project';

/**
 * Arbitrary generator for node IDs
 */
const arbNodeId = fc.string({ minLength: 1, maxLength: 20 }).filter(s => /^[a-zA-Z0-9_]+$/.test(s));

/**
 * Arbitrary generator for agent types
 */
const arbAgentType = fc.constantFrom<AgentSchema['type']>('llm', 'sequential', 'parallel', 'loop', 'router');

/**
 * Arbitrary generator for position
 */
const arbPosition = fc.record({
  x: fc.integer({ min: 0, max: 1000 }),
  y: fc.integer({ min: 0, max: 1000 }),
});

/**
 * Arbitrary generator for a minimal agent schema
 */
const arbAgentSchema: fc.Arbitrary<AgentSchema> = fc.record({
  type: arbAgentType,
  model: fc.option(fc.constantFrom('gemini-2.0-flash', 'gpt-4', 'claude-3'), { nil: undefined }),
  instruction: fc.string({ minLength: 0, maxLength: 100 }),
  tools: fc.array(fc.string({ minLength: 1, maxLength: 20 }), { maxLength: 5 }),
  sub_agents: fc.array(arbNodeId, { maxLength: 3 }),
  position: arbPosition,
  max_iterations: fc.option(fc.integer({ min: 1, max: 10 }), { nil: undefined }),
});

/**
 * Arbitrary generator for an edge
 */
const arbEdge: fc.Arbitrary<Edge> = fc.record({
  from: arbNodeId,
  to: arbNodeId,
});

/**
 * Arbitrary generator for an array of edges
 */
const arbEdges = fc.array(arbEdge, { maxLength: 5 });

/**
 * Arbitrary generator for an undoable action
 */
const arbUndoableAction: fc.Arbitrary<UndoableAction> = fc.record({
  type: fc.constantFrom<'add_node' | 'delete_node'>('add_node', 'delete_node'),
  timestamp: fc.nat({ max: Date.now() }),
  nodeId: arbNodeId,
  agent: arbAgentSchema,
  edges: arbEdges,
  edgesBefore: arbEdges,
});

/**
 * Helper to reset the store state completely
 */
function resetStore() {
  useUndoRedoStore.setState({
    undoStack: [],
    redoStack: [],
    canUndo: false,
    canRedo: false,
  });
}

/**
 * Reset the store before each test
 */
beforeEach(() => {
  resetStore();
});

describe('Undo/Redo Round-Trip', () => {
  /**
   * **Property 27: Undo/Redo Round-Trip** (CRITICAL)
   * *For any* add_node action, undoing and then redoing SHALL restore the exact same action.
   * **Validates: Requirements 11.5, 11.6**
   */
  it('should restore exact state after undo then redo for add_node', () => {
    fc.assert(
      fc.property(
        arbNodeId,
        arbAgentSchema,
        arbEdges,
        arbEdges,
        (nodeId, agent, edges, edgesBefore) => {
          // Reset store at start of each property iteration
          resetStore();
          
          const store = useUndoRedoStore.getState();
          
          // Record an add_node action
          store.recordAddNode(nodeId, agent, edges, edgesBefore);
          
          // Re-get state after mutation
          const stateAfterRecord = useUndoRedoStore.getState();
          
          // Verify action was recorded
          expect(stateAfterRecord.canUndo).toBe(true);
          expect(stateAfterRecord.undoStack.length).toBe(1);
          
          // Pop the action (simulating undo)
          const undoneAction = stateAfterRecord.popUndo();
          expect(undoneAction).not.toBeNull();
          expect(undoneAction!.type).toBe('add_node');
          expect(undoneAction!.nodeId).toBe(nodeId);
          
          // Push to redo stack
          useUndoRedoStore.getState().pushRedo(undoneAction!);
          expect(useUndoRedoStore.getState().canRedo).toBe(true);
          
          // Pop from redo stack (simulating redo)
          const redoneAction = useUndoRedoStore.getState().popRedo();
          expect(redoneAction).not.toBeNull();
          
          // Property: Redone action should match original
          expect(redoneAction!.type).toBe('add_node');
          expect(redoneAction!.nodeId).toBe(nodeId);
          expect(redoneAction!.agent.type).toBe(agent.type);
          expect(redoneAction!.agent.instruction).toBe(agent.instruction);
          expect(redoneAction!.edges.length).toBe(edges.length);
          expect(redoneAction!.edgesBefore.length).toBe(edgesBefore.length);
        }
      ),
      { numRuns: 100 }
    );
  });

  /**
   * Property: Undo/redo round-trip for delete_node should restore exact state
   */
  it('should restore exact state after undo then redo for delete_node', () => {
    fc.assert(
      fc.property(
        arbNodeId,
        arbAgentSchema,
        arbEdges,
        arbEdges,
        (nodeId, agent, edges, edgesBefore) => {
          // Reset store at start of each property iteration
          resetStore();
          
          const store = useUndoRedoStore.getState();
          
          // Record a delete_node action
          store.recordDeleteNode(nodeId, agent, edges, edgesBefore);
          
          // Re-get state after mutation
          const stateAfterRecord = useUndoRedoStore.getState();
          
          // Verify action was recorded
          expect(stateAfterRecord.canUndo).toBe(true);
          expect(stateAfterRecord.undoStack.length).toBe(1);
          
          // Pop the action (simulating undo)
          const undoneAction = stateAfterRecord.popUndo();
          expect(undoneAction).not.toBeNull();
          expect(undoneAction!.type).toBe('delete_node');
          expect(undoneAction!.nodeId).toBe(nodeId);
          
          // Push to redo stack
          useUndoRedoStore.getState().pushRedo(undoneAction!);
          expect(useUndoRedoStore.getState().canRedo).toBe(true);
          
          // Pop from redo stack (simulating redo)
          const redoneAction = useUndoRedoStore.getState().popRedo();
          expect(redoneAction).not.toBeNull();
          
          // Property: Redone action should match original
          expect(redoneAction!.type).toBe('delete_node');
          expect(redoneAction!.nodeId).toBe(nodeId);
          expect(redoneAction!.agent.type).toBe(agent.type);
          expect(redoneAction!.agent.instruction).toBe(agent.instruction);
          expect(redoneAction!.edges.length).toBe(edges.length);
          expect(redoneAction!.edgesBefore.length).toBe(edgesBefore.length);
        }
      ),
      { numRuns: 100 }
    );
  });

  /**
   * Property: Multiple undo/redo operations should maintain LIFO order
   */
  it('should maintain LIFO order for multiple undo/redo operations', () => {
    fc.assert(
      fc.property(
        fc.array(arbUndoableAction, { minLength: 2, maxLength: 10 }),
        (actions) => {
          // Reset store at start of each property iteration
          resetStore();
          
          // Record all actions
          actions.forEach((action) => {
            if (action.type === 'add_node') {
              useUndoRedoStore.getState().recordAddNode(action.nodeId, action.agent, action.edges, action.edgesBefore);
            } else {
              useUndoRedoStore.getState().recordDeleteNode(action.nodeId, action.agent, action.edges, action.edgesBefore);
            }
          });
          
          // Undo all actions and verify LIFO order
          const undoneActions: UndoableAction[] = [];
          for (let i = actions.length - 1; i >= 0; i--) {
            const undone = useUndoRedoStore.getState().popUndo();
            expect(undone).not.toBeNull();
            expect(undone!.nodeId).toBe(actions[i].nodeId);
            undoneActions.push(undone!);
            useUndoRedoStore.getState().pushRedo(undone!);
          }
          
          // Redo all actions and verify LIFO order (reverse of undo)
          for (let i = undoneActions.length - 1; i >= 0; i--) {
            const redone = useUndoRedoStore.getState().popRedo();
            expect(redone).not.toBeNull();
            expect(redone!.nodeId).toBe(undoneActions[i].nodeId);
            useUndoRedoStore.getState().pushUndo(redone!);
          }
        }
      ),
      { numRuns: 100 }
    );
  });

  /**
   * Property: Recording new action should clear redo stack
   */
  it('should clear redo stack when new action is recorded', () => {
    fc.assert(
      fc.property(
        arbNodeId,
        arbAgentSchema,
        arbEdges,
        arbEdges,
        arbNodeId,
        arbAgentSchema,
        arbEdges,
        arbEdges,
        (nodeId1, agent1, edges1, edgesBefore1, nodeId2, agent2, edges2, edgesBefore2) => {
          // Reset store at start of each property iteration
          resetStore();
          
          // Record first action
          useUndoRedoStore.getState().recordAddNode(nodeId1, agent1, edges1, edgesBefore1);
          
          // Undo it
          const undone = useUndoRedoStore.getState().popUndo();
          useUndoRedoStore.getState().pushRedo(undone!);
          expect(useUndoRedoStore.getState().canRedo).toBe(true);
          
          // Record new action
          useUndoRedoStore.getState().recordAddNode(nodeId2, agent2, edges2, edgesBefore2);
          
          // Property: Redo stack should be cleared
          const finalState = useUndoRedoStore.getState();
          expect(finalState.canRedo).toBe(false);
          expect(finalState.redoStack.length).toBe(0);
        }
      ),
      { numRuns: 100 }
    );
  });
});

describe('History Size Limits', () => {
  /**
   * Property: History should never exceed MAX_HISTORY_SIZE
   */
  it('should limit undo stack to MAX_HISTORY_SIZE', () => {
    fc.assert(
      fc.property(
        fc.array(arbUndoableAction, { minLength: MAX_HISTORY_SIZE + 1, maxLength: MAX_HISTORY_SIZE + 20 }),
        (actions) => {
          // Reset store at start of each property iteration
          resetStore();
          
          // Record more actions than MAX_HISTORY_SIZE
          actions.forEach((action) => {
            if (action.type === 'add_node') {
              useUndoRedoStore.getState().recordAddNode(action.nodeId, action.agent, action.edges, action.edgesBefore);
            } else {
              useUndoRedoStore.getState().recordDeleteNode(action.nodeId, action.agent, action.edges, action.edgesBefore);
            }
          });
          
          const finalState = useUndoRedoStore.getState();
          
          // Property: Undo stack should not exceed MAX_HISTORY_SIZE
          expect(finalState.undoStack.length).toBeLessThanOrEqual(MAX_HISTORY_SIZE);
          
          // Property: Most recent actions should be retained
          const lastAction = actions[actions.length - 1];
          const lastInStack = finalState.undoStack[finalState.undoStack.length - 1];
          expect(lastInStack.nodeId).toBe(lastAction.nodeId);
        }
      ),
      { numRuns: 100 }
    );
  });

  /**
   * Property: Redo stack should also be limited to MAX_HISTORY_SIZE
   */
  it('should limit redo stack to MAX_HISTORY_SIZE', () => {
    fc.assert(
      fc.property(
        fc.array(arbUndoableAction, { minLength: MAX_HISTORY_SIZE + 1, maxLength: MAX_HISTORY_SIZE + 20 }),
        (actions) => {
          // Reset store at start of each property iteration
          resetStore();
          
          // Push more actions to redo stack than MAX_HISTORY_SIZE
          actions.forEach((action) => {
            useUndoRedoStore.getState().pushRedo(action);
          });
          
          const finalState = useUndoRedoStore.getState();
          
          // Property: Redo stack should not exceed MAX_HISTORY_SIZE
          expect(finalState.redoStack.length).toBeLessThanOrEqual(MAX_HISTORY_SIZE);
        }
      ),
      { numRuns: 100 }
    );
  });
});

describe('Edge Cases', () => {
  /**
   * Property: Undo on empty stack should return null
   */
  it('should return null when undoing with empty stack', () => {
    resetStore();
    
    const state = useUndoRedoStore.getState();
    expect(state.canUndo).toBe(false);
    expect(state.popUndo()).toBeNull();
  });

  /**
   * Property: Redo on empty stack should return null
   */
  it('should return null when redoing with empty stack', () => {
    resetStore();
    
    const state = useUndoRedoStore.getState();
    expect(state.canRedo).toBe(false);
    expect(state.popRedo()).toBeNull();
  });

  /**
   * Property: Clear history should reset all state
   */
  it('should reset all state when clearing history', () => {
    fc.assert(
      fc.property(
        fc.array(arbUndoableAction, { minLength: 1, maxLength: 10 }),
        (actions) => {
          // Reset store at start of each property iteration
          resetStore();
          
          // Record some actions
          actions.forEach((action) => {
            if (action.type === 'add_node') {
              useUndoRedoStore.getState().recordAddNode(action.nodeId, action.agent, action.edges, action.edgesBefore);
            } else {
              useUndoRedoStore.getState().recordDeleteNode(action.nodeId, action.agent, action.edges, action.edgesBefore);
            }
          });
          
          // Clear history
          useUndoRedoStore.getState().clearHistory();
          
          const finalState = useUndoRedoStore.getState();
          
          // Property: All stacks should be empty
          expect(finalState.undoStack.length).toBe(0);
          expect(finalState.redoStack.length).toBe(0);
          expect(finalState.canUndo).toBe(false);
          expect(finalState.canRedo).toBe(false);
        }
      ),
      { numRuns: 100 }
    );
  });

  /**
   * Property: Agent data should be preserved exactly through undo/redo
   */
  it('should preserve agent data exactly through undo/redo cycle', () => {
    fc.assert(
      fc.property(
        arbNodeId,
        arbAgentSchema,
        arbEdges,
        arbEdges,
        (nodeId, agent, edges, edgesBefore) => {
          // Reset store at start of each property iteration
          resetStore();
          
          // Record action
          useUndoRedoStore.getState().recordAddNode(nodeId, agent, edges, edgesBefore);
          
          // Undo
          const undone = useUndoRedoStore.getState().popUndo();
          useUndoRedoStore.getState().pushRedo(undone!);
          
          // Redo
          const redone = useUndoRedoStore.getState().popRedo();
          
          // Property: All agent fields should be preserved
          expect(redone!.agent.type).toBe(agent.type);
          expect(redone!.agent.model).toBe(agent.model);
          expect(redone!.agent.instruction).toBe(agent.instruction);
          expect(redone!.agent.tools).toEqual(agent.tools);
          expect(redone!.agent.sub_agents).toEqual(agent.sub_agents);
          expect(redone!.agent.position.x).toBe(agent.position.x);
          expect(redone!.agent.position.y).toBe(agent.position.y);
          expect(redone!.agent.max_iterations).toBe(agent.max_iterations);
        }
      ),
      { numRuns: 100 }
    );
  });

  /**
   * Property: Edge data should be preserved exactly through undo/redo
   */
  it('should preserve edge data exactly through undo/redo cycle', () => {
    fc.assert(
      fc.property(
        arbNodeId,
        arbAgentSchema,
        arbEdges,
        arbEdges,
        (nodeId, agent, edges, edgesBefore) => {
          // Reset store at start of each property iteration
          resetStore();
          
          // Record action
          useUndoRedoStore.getState().recordDeleteNode(nodeId, agent, edges, edgesBefore);
          
          // Undo
          const undone = useUndoRedoStore.getState().popUndo();
          useUndoRedoStore.getState().pushRedo(undone!);
          
          // Redo
          const redone = useUndoRedoStore.getState().popRedo();
          
          // Property: All edges should be preserved
          expect(redone!.edges.length).toBe(edges.length);
          edges.forEach((edge, i) => {
            expect(redone!.edges[i].from).toBe(edge.from);
            expect(redone!.edges[i].to).toBe(edge.to);
          });
          
          // Property: edgesBefore should also be preserved
          expect(redone!.edgesBefore.length).toBe(edgesBefore.length);
          edgesBefore.forEach((edge, i) => {
            expect(redone!.edgesBefore[i].from).toBe(edge.from);
            expect(redone!.edgesBefore[i].to).toBe(edge.to);
          });
        }
      ),
      { numRuns: 100 }
    );
  });
});

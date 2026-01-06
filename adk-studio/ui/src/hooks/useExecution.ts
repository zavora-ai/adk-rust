import { useCallback, useState } from 'react';
import type { ExecutionState, ToolCall } from '../types/execution';

const initialState: ExecutionState = {
  isRunning: false,
  activeNode: null,
  activeSubAgent: null,
  thoughts: {},
  toolCalls: [],
  iteration: 0,
  startTime: null,
};

export function useExecution() {
  const [state, setState] = useState<ExecutionState>(initialState);

  const start = useCallback(() => {
    setState({ ...initialState, isRunning: true, startTime: Date.now() });
  }, []);

  const stop = useCallback(() => {
    setState(s => ({ ...s, isRunning: false }));
  }, []);

  const setActiveNode = useCallback((nodeId: string | null, subAgent?: string) => {
    setState(s => ({ ...s, activeNode: nodeId, activeSubAgent: subAgent || null }));
  }, []);

  const setThought = useCallback((nodeId: string, thought: string) => {
    setState(s => ({ ...s, thoughts: { ...s.thoughts, [nodeId]: thought } }));
  }, []);

  const clearThought = useCallback((nodeId: string) => {
    setState(s => {
      const { [nodeId]: _, ...rest } = s.thoughts;
      return { ...s, thoughts: rest };
    });
  }, []);

  const addToolCall = useCallback((tc: Omit<ToolCall, 'status'>) => {
    setState(s => ({ ...s, toolCalls: [...s.toolCalls, { ...tc, status: 'running' }] }));
  }, []);

  const completeToolCall = useCallback((id: string, result: unknown) => {
    setState(s => ({
      ...s,
      toolCalls: s.toolCalls.map(tc => tc.id === id ? { ...tc, result, status: 'complete' } : tc),
    }));
  }, []);

  const incrementIteration = useCallback(() => {
    setState(s => ({ ...s, iteration: s.iteration + 1 }));
  }, []);

  const reset = useCallback(() => setState(initialState), []);

  return { ...state, start, stop, setActiveNode, setThought, clearThought, addToolCall, completeToolCall, incrementIteration, reset };
}

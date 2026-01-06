export interface ToolCall {
  id: string;
  name: string;
  args: unknown;
  result?: unknown;
  status: 'pending' | 'running' | 'complete' | 'error';
}

export interface ExecutionState {
  isRunning: boolean;
  activeNode: string | null;
  activeSubAgent: string | null;
  thoughts: Record<string, string>;
  toolCalls: ToolCall[];
  iteration: number;
  startTime: number | null;
}

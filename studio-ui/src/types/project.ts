export interface Project {
  id: string;
  version: string;
  name: string;
  description: string;
  settings: ProjectSettings;
  agents: Record<string, AgentSchema>;
  tools: Record<string, ToolSchema>;
  workflow: WorkflowSchema;
  created_at: string;
  updated_at: string;
}

export interface ProjectSettings {
  default_model: string;
  env_vars: Record<string, string>;
}

export interface AgentSchema {
  type: 'llm' | 'tool' | 'sequential' | 'parallel' | 'loop' | 'graph' | 'custom';
  model?: string;
  instruction: string;
  tools: string[];
  sub_agents: string[];
  position: Position;
}

export interface ToolSchema {
  type: 'builtin' | 'mcp' | 'custom';
  config: Record<string, unknown>;
  description: string;
}

export interface WorkflowSchema {
  type: 'single' | 'sequential' | 'parallel' | 'graph';
  edges: Edge[];
  conditions: Condition[];
}

export interface Edge {
  from: string;
  to: string;
  condition?: string;
}

export interface Condition {
  id: string;
  expression: string;
  description: string;
}

export interface Position {
  x: number;
  y: number;
}

export interface ProjectMeta {
  id: string;
  name: string;
  description: string;
  updated_at: string;
}

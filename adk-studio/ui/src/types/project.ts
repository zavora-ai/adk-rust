export interface Project {
  id: string;
  version: string;
  name: string;
  description: string;
  settings: ProjectSettings;
  agents: Record<string, AgentSchema>;
  tools: Record<string, ToolSchema>;
  tool_configs: Record<string, ToolConfig>;
  workflow: WorkflowSchema;
  created_at: string;
  updated_at: string;
}

export interface ProjectSettings {
  default_model: string;
  env_vars: Record<string, string>;
}

export interface AgentSchema {
  type: 'llm' | 'tool' | 'sequential' | 'parallel' | 'loop' | 'router' | 'graph' | 'custom';
  // Basic
  model?: string;
  instruction: string;
  description?: string;
  // Advanced
  global_instruction?: string;
  output_key?: string;
  output_schema?: string; // JSON schema string
  // Behavior
  include_contents?: 'all' | 'none' | 'last';
  disallow_transfer_to_parent?: boolean;
  disallow_transfer_to_peers?: boolean;
  // Relations
  tools: string[];
  sub_agents: string[];
  position: Position;
  // Container-specific
  max_iterations?: number;
  routes?: Route[];
}

export interface Route {
  condition: string;
  target: string;
}

export interface ToolSchema {
  type: 'builtin' | 'mcp' | 'custom';
  config: Record<string, unknown>;
  description: string;
}

// Tool configurations
export type ToolConfig = McpToolConfig | FunctionToolConfig | BrowserToolConfig;

export interface McpToolConfig {
  type: 'mcp';
  name?: string;
  server_command: string;
  server_args: string[];
  tool_filter?: string[];
}

export interface FunctionToolConfig {
  type: 'function';
  name: string;
  description: string;
  parameters: FunctionParameter[];
  code?: string;
}

export interface FunctionParameter {
  name: string;
  param_type: 'string' | 'number' | 'boolean';
  description: string;
  required: boolean;
}

export interface BrowserToolConfig {
  type: 'browser';
  headless: boolean;
  timeout_ms: number;
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

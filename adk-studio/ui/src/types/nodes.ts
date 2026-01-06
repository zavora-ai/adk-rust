// Node data types for custom React Flow nodes

export interface BaseNodeData {
  label: string;
  isActive?: boolean;
  thought?: string;
  [key: string]: unknown; // Allow additional properties
}

export interface LlmNodeData extends BaseNodeData {
  model?: string;
  instruction?: string;
  tools?: string[];
}

export interface ContainerNodeData extends BaseNodeData {
  subAgents?: string[];
  activeSubAgent?: string;
}

export interface LoopNodeData extends ContainerNodeData {
  maxIterations?: number;
  currentIteration?: number;
}

export interface RouterNodeData extends BaseNodeData {
  model?: string;
  routes?: Array<{ condition: string; target: string }>;
  activeRoute?: string;
}

// Node data types for custom React Flow nodes

/**
 * Base data interface for all node types.
 * All custom nodes should extend this interface.
 */
export interface BaseNodeData {
  label: string;
  isActive?: boolean;
  thought?: string;
  [key: string]: unknown; // Allow additional properties
}

/**
 * Data for LLM Agent nodes.
 */
export interface LlmNodeData extends BaseNodeData {
  model?: string;
  instruction?: string;
  tools?: string[];
}

/**
 * Data for container nodes (Sequential, Parallel, Loop).
 */
export interface ContainerNodeData extends BaseNodeData {
  subAgents?: string[];
  subAgentTools?: Record<string, string[]>;
  activeSubAgent?: string;
}

/**
 * Data for Loop nodes.
 */
export interface LoopNodeData extends ContainerNodeData {
  maxIterations?: number;
  currentIteration?: number;
}

/**
 * Data for Router nodes.
 */
export interface RouterNodeData extends BaseNodeData {
  model?: string;
  routes?: Array<{ condition: string; target: string }>;
  activeRoute?: string;
}

/**
 * Data for Start/End nodes.
 */
// eslint-disable-next-line @typescript-eslint/no-empty-object-type
export interface StartEndNodeData {
  // Start/End nodes have no configurable data
}

/**
 * Union type of all node data types.
 * Use this when you need to handle any node type.
 */
export type AnyNodeData = 
  | LlmNodeData 
  | ContainerNodeData 
  | LoopNodeData 
  | RouterNodeData 
  | StartEndNodeData;

/**
 * Node type identifiers.
 * These must match the keys in nodeTypes registration.
 */
export type NodeType = 
  | 'llm' 
  | 'sequential' 
  | 'loop' 
  | 'parallel' 
  | 'router' 
  | 'start' 
  | 'end';

/**
 * Edge type identifiers.
 * These must match the keys in edgeTypes registration.
 */
export type EdgeType = 
  | 'animated' 
  | 'dataflow';

/**
 * Data for animated edges.
 */
export interface AnimatedEdgeData {
  animated?: boolean;
}

/**
 * Data for data flow edges (v2.0).
 * State keys are populated from runtime SSE events.
 * Note: This interface is also defined locally in DataFlowEdge.tsx
 * to avoid type compatibility issues with ReactFlow's EdgeProps.
 */
export interface DataFlowEdgeData {
  animated?: boolean;
  stateKeys?: string[];
  showOverlay?: boolean;
}

/**
 * Union type of all edge data types.
 */
export type AnyEdgeData = AnimatedEdgeData | DataFlowEdgeData;

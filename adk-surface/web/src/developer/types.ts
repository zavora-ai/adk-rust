export type Capabilities = {
  runtimeTools: boolean;
  handoff: boolean;
  relationshipConfirmation: boolean;
  checkpointResume: boolean;
  sharedState: boolean;
  invocationMetadata: boolean;
};

export type TopologyMember = {
  name: string;
  description: string;
  coordinator: boolean;
  capabilities: Capabilities;
};

export type TopologyRelationship = {
  from: string;
  to: string;
  kind: "flow" | "delegate" | "handoff";
};

export type AgentDetails = {
  name: string;
  description: string;
  kind: "agent" | "composite" | "team" | "workflow" | "realtime";
  interactionMode: "requestResponse" | "realtime";
  capabilities: Capabilities;
  services: {
    telemetry: boolean;
    telemetryStatus?: "disabled" | "configured" | "collecting";
    artifacts: boolean;
    memory: boolean;
  };
  children: Array<{ name: string; description: string; capabilities: Capabilities }>;
  topology?: {
    root: string;
    coordinator: string;
    members: TopologyMember[];
    relationships: TopologyRelationship[];
  };
};

export type EventPart = Record<string, unknown>;
export type RuntimeEvent = {
  id: string;
  timestamp?: string;
  invocation_id?: string;
  branch?: string;
  author?: string;
  content?: { role?: string; parts?: EventPart[] };
  partial?: boolean;
  turn_complete?: boolean;
  interrupted?: boolean;
  error_code?: string | null;
  error_message?: string | null;
  actions?: {
    state_delta?: Record<string, unknown>;
    artifact_delta?: Record<string, number>;
    transfer_to_agent?: string | null;
    escalate?: boolean;
    tool_confirmation?: unknown;
  };
  event_metadata?: Record<string, string>;
};

export type Session = {
  id: string;
  appName: string;
  userId: string;
  lastUpdateTime: number;
  events: RuntimeEvent[];
  state: Record<string, unknown>;
};

export type UiCapabilities = {
  protocols?: Array<string | {
    protocol: string;
    versions?: string[];
    implementationTier?: string;
    summary?: string;
    features?: string[];
    limitations?: string[];
    deprecation?: { stage?: string; sunsetTargetOn?: string };
  }>;
  supportedProtocols?: string[];
  capabilities?: Record<string, unknown>;
  [key: string]: unknown;
};

export type A2aCard = {
  name?: string;
  description?: string;
  url?: string;
  version?: string;
  capabilities?: Record<string, unknown>;
  skills?: Array<{ id?: string; name?: string; description?: string }>;
  [key: string]: unknown;
};

export type TraceSpan = {
  name: string;
  span_id: string;
  trace_id: string;
  start_time: number;
  end_time: number;
  attributes: Record<string, string>;
  invoc_id?: string;
};

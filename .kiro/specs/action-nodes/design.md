# Design Document: Action Nodes

## Overview

Action Nodes extend ADK Studio with non-LLM programmatic nodes for deterministic operations in workflows. Inspired by n8n's approach, these nodes complement LLM agents by handling data transformation, API integrations, control flow, and automation logic.

### Design Principles

1. **Composability**: Action nodes work seamlessly with LLM agents
2. **Determinism**: Predictable, repeatable behavior for automation
3. **Visual Clarity**: Distinct appearance from LLM agents
4. **Code Generation**: All nodes generate valid Rust code
5. **Extensibility**: Standard properties enable consistent behavior

### Technology Stack

| Component | Technology | Notes |
|-----------|------------|-------|
| Frontend | React 18 + TypeScript | Existing ADK Studio |
| Canvas | ReactFlow (@xyflow/react) | Existing |
| State | Zustand | Existing |
| Backend | Rust (Axum) | Existing adk-studio crate |
| HTTP Client | reqwest | For HTTP node code gen |
| Database | sqlx, mongodb | For Database node code gen |
| Sandbox | quickjs-rs | For Code node execution |

## Architecture

### High-Level Architecture

```
┌─────────────────────────────────────────────────────────────────────────┐
│                         ADK Studio Frontend                              │
├─────────────────────────────────────────────────────────────────────────┤
│  ┌─────────────────────────────────────────────────────────────────┐    │
│  │                     Action Node Components                       │    │
│  │  ┌─────────┐ ┌─────────┐ ┌─────────┐ ┌─────────┐ ┌─────────┐   │    │
│  │  │ Trigger │ │  HTTP   │ │   Set   │ │Transform│ │ Switch  │   │    │
│  │  └─────────┘ └─────────┘ └─────────┘ └─────────┘ └─────────┘   │    │
│  │  ┌─────────┐ ┌─────────┐ ┌─────────┐ ┌─────────┐ ┌─────────┐   │    │
│  │  │  Loop   │ │  Merge  │ │  Wait   │ │  Code   │ │Database │   │    │
│  │  └─────────┘ └─────────┘ └─────────┘ └─────────┘ └─────────┘   │    │
│  └─────────────────────────────────────────────────────────────────┘    │
│                                │                                        │
│  ┌────────────────────────────┴────────────────────────────────────┐   │
│  │                    Shared Components                             │   │
│  │  ActionNodeBase │ PropertiesPanel │ CodeEditor │ JsonEditor     │   │
│  └─────────────────────────────────────────────────────────────────┘   │
│                                │                                        │
│  ┌────────────────────────────┴────────────────────────────────────┐   │
│  │                      Store Layer (Zustand)                       │   │
│  │  actionNodesSlice │ executionSlice │ projectSlice               │   │
│  └─────────────────────────────────────────────────────────────────┘   │
└─────────────────────────────────────────────────────────────────────────┘
                                    │
                                    │ REST / SSE
                                    ▼
┌─────────────────────────────────────────────────────────────────────────┐
│                         ADK Studio Backend (Rust)                        │
│  ┌──────────────┐  ┌──────────────┐  ┌──────────────┐                  │
│  │   Action     │  │    Code      │  │   Runtime    │                  │
│  │   Executor   │  │  Generator   │  │   Sandbox    │                  │
│  └──────────────┘  └──────────────┘  └──────────────┘                  │
└─────────────────────────────────────────────────────────────────────────┘
```

### Component Architecture

```
src/
├── components/
│   ├── ActionNodes/
│   │   ├── index.ts                # Export all action node types
│   │   ├── ActionNodeBase.tsx      # Shared wrapper with standard props
│   │   ├── TriggerNode.tsx         # Entry point node
│   │   ├── HttpNode.tsx            # API calls
│   │   ├── SetNode.tsx             # Variable definition
│   │   ├── TransformNode.tsx       # Data transformation
│   │   ├── SwitchNode.tsx          # Conditional branching
│   │   ├── LoopNode.tsx            # Iteration
│   │   ├── MergeNode.tsx           # Branch combination
│   │   ├── WaitNode.tsx            # Delays and timing
│   │   ├── CodeNode.tsx            # Custom JavaScript
│   │   └── DatabaseNode.tsx        # SQL/NoSQL operations
│   │
│   ├── ActionPanels/
│   │   ├── StandardPropertiesPanel.tsx  # Error handling, tracing, etc.
│   │   ├── TriggerPanel.tsx
│   │   ├── HttpPanel.tsx
│   │   ├── SetPanel.tsx
│   │   ├── TransformPanel.tsx
│   │   ├── SwitchPanel.tsx
│   │   ├── LoopPanel.tsx
│   │   ├── MergePanel.tsx
│   │   ├── WaitPanel.tsx
│   │   ├── CodePanel.tsx
│   │   └── DatabasePanel.tsx
│   │
│   └── ActionPalette/
│       └── ActionPalette.tsx       # Draggable action node types
│
├── types/
│   ├── actionNodes.ts              # All action node type definitions
│   └── standardProperties.ts       # Standard property interfaces
│
└── codegen/
    ├── actionCodegen.ts            # Action node code generation
    └── templates/                  # Rust code templates
        ├── trigger.rs.hbs
        ├── http.rs.hbs
        ├── set.rs.hbs
        ├── transform.rs.hbs
        ├── switch.rs.hbs
        ├── loop.rs.hbs
        ├── merge.rs.hbs
        ├── wait.rs.hbs
        ├── code.rs.hbs
        └── database.rs.hbs
```

## Components and Interfaces

### Standard Properties Interface

All action nodes share these standard properties:

```typescript
// types/standardProperties.ts

export interface StandardProperties {
  // Identity
  id: string;
  name: string;
  description?: string;
  
  // Error Handling
  errorHandling: {
    mode: 'stop' | 'continue' | 'retry' | 'fallback';
    retryCount?: number;      // 1-10, for retry mode
    retryDelay?: number;      // ms, for retry mode
    fallbackValue?: unknown;  // for fallback mode
  };
  
  // Tracing & Observability
  tracing: {
    enabled: boolean;
    logLevel: 'none' | 'error' | 'info' | 'debug';
  };
  
  // Callbacks
  callbacks: {
    onStart?: string;    // Function name or inline code
    onComplete?: string;
    onError?: string;
  };
  
  // Execution Control
  execution: {
    timeout: number;     // ms, default 30000
    condition?: string;  // Expression to skip if false
  };
  
  // Input/Output Mapping
  mapping: {
    inputMapping?: Record<string, string>;  // state field -> node input
    outputKey: string;                       // where to store result
  };
}
```

### Action Node Type Definitions

```typescript
// types/actionNodes.ts

import { StandardProperties } from './standardProperties';

// ============================================
// 1. TRIGGER NODE
// ============================================
export interface TriggerNodeConfig extends StandardProperties {
  type: 'trigger';
  triggerType: 'manual' | 'webhook' | 'schedule' | 'event';
  
  // Webhook config
  webhook?: {
    path: string;
    method: 'GET' | 'POST';
    auth: 'none' | 'bearer' | 'api_key';
    authConfig?: {
      headerName?: string;
      tokenEnvVar?: string;
    };
  };
  
  // Schedule config
  schedule?: {
    cron: string;
    timezone: string;
  };
  
  // Event config
  event?: {
    source: string;
    eventType: string;
  };
}

// ============================================
// 2. HTTP NODE
// ============================================
export interface HttpNodeConfig extends StandardProperties {
  type: 'http';
  method: 'GET' | 'POST' | 'PUT' | 'PATCH' | 'DELETE';
  url: string;  // Supports {{variable}} interpolation
  
  auth: {
    type: 'none' | 'bearer' | 'basic' | 'api_key';
    bearer?: { token: string };
    basic?: { username: string; password: string };
    apiKey?: { headerName: string; value: string };
  };
  
  headers: Record<string, string>;
  
  body: {
    type: 'none' | 'json' | 'form' | 'raw';
    content?: string | Record<string, unknown>;
  };
  
  response: {
    type: 'json' | 'text' | 'binary';
    statusValidation?: string;  // e.g., "200-299"
    jsonPath?: string;          // Extract specific field
  };
  
  rateLimit?: {
    requestsPerWindow: number;
    windowMs: number;
  };
}

// ============================================
// 3. SET NODE
// ============================================
export interface SetNodeConfig extends StandardProperties {
  type: 'set';
  mode: 'set' | 'merge' | 'delete';
  
  variables: Array<{
    key: string;
    value: string | number | boolean | object;
    valueType: 'string' | 'number' | 'boolean' | 'json' | 'expression';
    isSecret: boolean;
  }>;
  
  envVars?: {
    loadFromEnv: boolean;
    prefix?: string;
  };
}

// ============================================
// 4. TRANSFORM NODE
// ============================================
export interface TransformNodeConfig extends StandardProperties {
  type: 'transform';
  transformType: 'jsonpath' | 'jmespath' | 'template' | 'javascript';
  
  expression: string;  // The transformation expression
  
  // Built-in operations (alternative to expression)
  operations?: Array<{
    type: 'pick' | 'omit' | 'rename' | 'flatten' | 'sort' | 'unique';
    config: Record<string, unknown>;
  }>;
  
  typeCoercion?: {
    targetType: 'string' | 'number' | 'boolean' | 'array' | 'object';
  };
}

// ============================================
// 5. SWITCH NODE
// ============================================
export interface SwitchNodeConfig extends StandardProperties {
  type: 'switch';
  evaluationMode: 'first_match' | 'all_match';
  
  conditions: Array<{
    id: string;
    name: string;
    field: string;
    operator: 'eq' | 'neq' | 'gt' | 'lt' | 'gte' | 'lte' | 
              'contains' | 'startsWith' | 'endsWith' | 
              'matches' | 'in' | 'empty' | 'exists';
    value?: unknown;
    outputPort: string;  // Which output port to use
  }>;
  
  defaultBranch?: string;  // Output port for no match
  
  // Expression mode (alternative to conditions)
  expressionMode?: {
    enabled: boolean;
    expression: string;  // Returns branch name
  };
}

// ============================================
// 6. LOOP NODE
// ============================================
export interface LoopNodeConfig extends StandardProperties {
  type: 'loop';
  loopType: 'forEach' | 'while' | 'times';
  
  // forEach config
  forEach?: {
    sourceArray: string;  // Path to array in state
    itemVar: string;      // Default: 'item'
    indexVar: string;     // Default: 'index'
  };
  
  // while config
  while?: {
    condition: string;
  };
  
  // times config
  times?: {
    count: number | string;  // Number or expression
  };
  
  parallel: {
    enabled: boolean;
    batchSize?: number;
    delayBetween?: number;  // ms
  };
  
  results: {
    collect: boolean;
    aggregationKey?: string;
  };
}

// ============================================
// 7. MERGE NODE
// ============================================
export interface MergeNodeConfig extends StandardProperties {
  type: 'merge';
  mode: 'wait_all' | 'wait_any' | 'wait_n';
  waitCount?: number;  // For wait_n mode
  
  combineStrategy: 'array' | 'object' | 'first' | 'last';
  branchKeys?: string[];  // For object strategy
  
  timeout: {
    enabled: boolean;
    ms: number;
    behavior: 'continue' | 'error';
  };
}

// ============================================
// 8. WAIT NODE
// ============================================
export interface WaitNodeConfig extends StandardProperties {
  type: 'wait';
  waitType: 'fixed' | 'until' | 'webhook' | 'condition';
  
  // fixed config
  fixed?: {
    duration: number;
    unit: 'ms' | 's' | 'm' | 'h';
  };
  
  // until config
  until?: {
    timestamp: string;  // ISO string or expression
  };
  
  // webhook config
  webhook?: {
    path: string;
    timeout: number;
  };
  
  // condition config
  condition?: {
    expression: string;
    pollInterval: number;  // ms
    maxWait: number;       // ms
  };
}

// ============================================
// 9. CODE NODE
// ============================================
export interface CodeNodeConfig extends StandardProperties {
  type: 'code';
  language: 'javascript' | 'typescript';
  code: string;
  
  sandbox: {
    networkAccess: boolean;
    fileSystemAccess: boolean;
    memoryLimit: number;    // MB
    timeLimit: number;      // ms
  };
  
  // Type hints for editor
  inputType?: string;   // TypeScript type definition
  outputType?: string;
}

// ============================================
// 10. DATABASE NODE
// ============================================
export interface DatabaseNodeConfig extends StandardProperties {
  type: 'database';
  dbType: 'postgresql' | 'mysql' | 'sqlite' | 'mongodb' | 'redis';
  
  connection: {
    connectionString: string;  // Marked as secret
    credentialRef?: string;    // Reference to Set node
    poolSize?: number;
  };
  
  // SQL operations
  sql?: {
    operation: 'query' | 'insert' | 'update' | 'delete' | 'upsert';
    query: string;
    params?: Record<string, unknown>;
  };
  
  // MongoDB operations
  mongodb?: {
    collection: string;
    operation: 'find' | 'findOne' | 'insert' | 'update' | 'delete';
    filter?: Record<string, unknown>;
    document?: Record<string, unknown>;
  };
  
  // Redis operations
  redis?: {
    operation: 'get' | 'set' | 'del' | 'hget' | 'hset' | 'lpush' | 'rpop';
    key: string;
    value?: unknown;
    ttl?: number;
  };
}

// Union type for all action nodes
export type ActionNodeConfig = 
  | TriggerNodeConfig
  | HttpNodeConfig
  | SetNodeConfig
  | TransformNodeConfig
  | SwitchNodeConfig
  | LoopNodeConfig
  | MergeNodeConfig
  | WaitNodeConfig
  | CodeNodeConfig
  | DatabaseNodeConfig;
```

### Visual Design

#### Node Appearance

Action nodes use a distinct visual style from LLM agents:

```typescript
// Visual constants for action nodes
export const ACTION_NODE_COLORS = {
  trigger: '#6366F1',    // Indigo - entry point
  http: '#3B82F6',       // Blue - network
  set: '#8B5CF6',        // Purple - variables
  transform: '#EC4899',  // Pink - data manipulation
  switch: '#F59E0B',     // Amber - branching
  loop: '#10B981',       // Emerald - iteration
  merge: '#06B6D4',      // Cyan - combination
  wait: '#6B7280',       // Gray - timing
  code: '#EF4444',       // Red - custom code
  database: '#14B8A6',   // Teal - storage
};

export const ACTION_NODE_ICONS = {
  trigger: '🎯',
  http: '🌐',
  set: '📝',
  transform: '⚙️',
  switch: '🔀',
  loop: '🔄',
  merge: '🔗',
  wait: '⏱️',
  code: '💻',
  database: '🗄️',
};
```

#### ActionNodeBase Component

```typescript
// components/ActionNodes/ActionNodeBase.tsx
interface ActionNodeBaseProps {
  id: string;
  type: ActionNodeConfig['type'];
  label: string;
  isActive: boolean;
  isSelected: boolean;
  status: 'idle' | 'running' | 'success' | 'error';
  hasError: boolean;
  children?: React.ReactNode;
  inputPorts?: number;
  outputPorts?: number;
}

export function ActionNodeBase({
  id, type, label, isActive, isSelected, status, hasError, children,
  inputPorts = 1, outputPorts = 1
}: ActionNodeBaseProps) {
  const { mode } = useTheme();
  const color = ACTION_NODE_COLORS[type];
  const icon = ACTION_NODE_ICONS[type];
  
  return (
    <div 
      className={cn(
        'action-node',
        `action-node-${mode}`,
        isActive && 'action-node-active',
        isSelected && 'action-node-selected',
        hasError && 'action-node-error',
        `action-node-status-${status}`
      )}
      style={{ '--action-color': color } as React.CSSProperties}
    >
      {/* Input handles */}
      {Array.from({ length: inputPorts }).map((_, i) => (
        <Handle
          key={`input-${i}`}
          type="target"
          position={Position.Top}
          id={`input-${i}`}
          style={{ left: `${(i + 1) * 100 / (inputPorts + 1)}%` }}
        />
      ))}
      
      {/* Header with icon and type badge */}
      <div className="action-node-header">
        <span className="action-node-icon">{icon}</span>
        <span className="action-node-label">{label}</span>
        <span className="action-node-type-badge">{type}</span>
        <StatusIndicator status={status} />
      </div>
      
      {/* Body content */}
      {children && (
        <div className="action-node-body">{children}</div>
      )}
      
      {/* Error indicator */}
      {hasError && (
        <div className="action-node-error-badge">!</div>
      )}
      
      {/* Output handles */}
      {Array.from({ length: outputPorts }).map((_, i) => (
        <Handle
          key={`output-${i}`}
          type="source"
          position={Position.Bottom}
          id={`output-${i}`}
          style={{ left: `${(i + 1) * 100 / (outputPorts + 1)}%` }}
        />
      ))}
    </div>
  );
}
```

### Properties Panel

```typescript
// components/ActionPanels/StandardPropertiesPanel.tsx
interface StandardPropertiesPanelProps {
  properties: StandardProperties;
  onChange: (props: StandardProperties) => void;
}

export function StandardPropertiesPanel({ properties, onChange }: StandardPropertiesPanelProps) {
  return (
    <div className="standard-properties-panel">
      {/* Error Handling Section */}
      <CollapsibleSection title="Error Handling" defaultOpen={false}>
        <Select
          label="Error Mode"
          value={properties.errorHandling.mode}
          options={['stop', 'continue', 'retry', 'fallback']}
          onChange={(mode) => onChange({
            ...properties,
            errorHandling: { ...properties.errorHandling, mode }
          })}
        />
        
        {properties.errorHandling.mode === 'retry' && (
          <>
            <NumberInput
              label="Retry Count"
              value={properties.errorHandling.retryCount || 3}
              min={1}
              max={10}
              onChange={(retryCount) => onChange({
                ...properties,
                errorHandling: { ...properties.errorHandling, retryCount }
              })}
            />
            <NumberInput
              label="Retry Delay (ms)"
              value={properties.errorHandling.retryDelay || 1000}
              min={0}
              onChange={(retryDelay) => onChange({
                ...properties,
                errorHandling: { ...properties.errorHandling, retryDelay }
              })}
            />
          </>
        )}
        
        {properties.errorHandling.mode === 'fallback' && (
          <JsonEditor
            label="Fallback Value"
            value={properties.errorHandling.fallbackValue}
            onChange={(fallbackValue) => onChange({
              ...properties,
              errorHandling: { ...properties.errorHandling, fallbackValue }
            })}
          />
        )}
      </CollapsibleSection>
      
      {/* Tracing Section */}
      <CollapsibleSection title="Tracing & Logging" defaultOpen={false}>
        <Toggle
          label="Enable Tracing"
          value={properties.tracing.enabled}
          onChange={(enabled) => onChange({
            ...properties,
            tracing: { ...properties.tracing, enabled }
          })}
        />
        <Select
          label="Log Level"
          value={properties.tracing.logLevel}
          options={['none', 'error', 'info', 'debug']}
          onChange={(logLevel) => onChange({
            ...properties,
            tracing: { ...properties.tracing, logLevel }
          })}
        />
      </CollapsibleSection>
      
      {/* Callbacks Section */}
      <CollapsibleSection title="Callbacks" defaultOpen={false}>
        <TextInput
          label="onStart"
          value={properties.callbacks.onStart || ''}
          placeholder="Function name or inline code"
          onChange={(onStart) => onChange({
            ...properties,
            callbacks: { ...properties.callbacks, onStart }
          })}
        />
        <TextInput
          label="onComplete"
          value={properties.callbacks.onComplete || ''}
          onChange={(onComplete) => onChange({
            ...properties,
            callbacks: { ...properties.callbacks, onComplete }
          })}
        />
        <TextInput
          label="onError"
          value={properties.callbacks.onError || ''}
          onChange={(onError) => onChange({
            ...properties,
            callbacks: { ...properties.callbacks, onError }
          })}
        />
      </CollapsibleSection>
      
      {/* Execution Control Section */}
      <CollapsibleSection title="Execution Control" defaultOpen={false}>
        <NumberInput
          label="Timeout (ms)"
          value={properties.execution.timeout}
          min={0}
          onChange={(timeout) => onChange({
            ...properties,
            execution: { ...properties.execution, timeout }
          })}
        />
        <TextInput
          label="Skip Condition"
          value={properties.execution.condition || ''}
          placeholder="Expression (skip if false)"
          onChange={(condition) => onChange({
            ...properties,
            execution: { ...properties.execution, condition }
          })}
        />
      </CollapsibleSection>
      
      {/* Input/Output Mapping Section */}
      <CollapsibleSection title="Input/Output Mapping" defaultOpen={true}>
        <KeyValueEditor
          label="Input Mapping"
          value={properties.mapping.inputMapping || {}}
          onChange={(inputMapping) => onChange({
            ...properties,
            mapping: { ...properties.mapping, inputMapping }
          })}
        />
        <TextInput
          label="Output Key"
          value={properties.mapping.outputKey}
          placeholder="State key for result"
          onChange={(outputKey) => onChange({
            ...properties,
            mapping: { ...properties.mapping, outputKey }
          })}
        />
      </CollapsibleSection>
    </div>
  );
}
```

## Code Generation

### Rust Code Templates

Each action node generates corresponding Rust code. Here are the key patterns:

#### HTTP Node Code Generation

```rust
// Generated code for HTTP node
use reqwest::Client;
use serde_json::Value;

async fn http_node_{{node_id}}(
    state: &mut State,
    client: &Client,
) -> Result<Value, ActionError> {
    let url = interpolate_variables("{{url}}", state);
    
    let mut request = client.{{method}}(&url);
    
    {{#if headers}}
    {{#each headers}}
    request = request.header("{{@key}}", interpolate_variables("{{this}}", state));
    {{/each}}
    {{/if}}
    
    {{#if auth.bearer}}
    request = request.bearer_auth(&state.get_secret("{{auth.bearer.token}}")?);
    {{/if}}
    
    {{#if body.json}}
    request = request.json(&serde_json::json!({{body.content}}));
    {{/if}}
    
    let response = request.send().await?;
    
    {{#if response.statusValidation}}
    let status = response.status().as_u16();
    if !validate_status(status, "{{response.statusValidation}}") {
        return Err(ActionError::HttpStatus(status));
    }
    {{/if}}
    
    let result: Value = response.json().await?;
    
    {{#if response.jsonPath}}
    let extracted = jsonpath::select(&result, "{{response.jsonPath}}")?;
    state.set("{{mapping.outputKey}}", extracted);
    {{else}}
    state.set("{{mapping.outputKey}}", result);
    {{/if}}
    
    Ok(state.get("{{mapping.outputKey}}").clone())
}
```

#### Switch Node Code Generation

```rust
// Generated code for Switch node
async fn switch_node_{{node_id}}(
    state: &State,
) -> Result<&'static str, ActionError> {
    {{#each conditions}}
    let value = state.get("{{field}}");
    if {{#switch operator}}
        {{#case "eq"}}value == &serde_json::json!({{value}}){{/case}}
        {{#case "contains"}}value.as_str().map(|s| s.contains("{{value}}")).unwrap_or(false){{/case}}
        {{#case "gt"}}value.as_f64().map(|n| n > {{value}}).unwrap_or(false){{/case}}
        // ... other operators
    {{/switch}} {
        return Ok("{{outputPort}}");
    }
    {{/each}}
    
    {{#if defaultBranch}}
    Ok("{{defaultBranch}}")
    {{else}}
    Err(ActionError::NoMatchingBranch)
    {{/if}}
}
```

#### Loop Node Code Generation

```rust
// Generated code for Loop node (forEach)
async fn loop_node_{{node_id}}(
    state: &mut State,
    executor: &WorkflowExecutor,
) -> Result<Vec<Value>, ActionError> {
    let source: Vec<Value> = state.get("{{forEach.sourceArray}}")
        .as_array()
        .ok_or(ActionError::InvalidArray)?
        .clone();
    
    let mut results = Vec::new();
    
    {{#if parallel.enabled}}
    let chunks: Vec<_> = source.chunks({{parallel.batchSize}}).collect();
    for chunk in chunks {
        let futures: Vec<_> = chunk.iter().enumerate().map(|(idx, item)| {
            let mut loop_state = state.clone();
            loop_state.set("{{forEach.itemVar}}", item.clone());
            loop_state.set("{{forEach.indexVar}}", idx);
            executor.execute_subgraph("{{loopBody}}", loop_state)
        }).collect();
        
        let chunk_results = futures::future::join_all(futures).await;
        results.extend(chunk_results.into_iter().filter_map(|r| r.ok()));
        
        {{#if parallel.delayBetween}}
        tokio::time::sleep(Duration::from_millis({{parallel.delayBetween}})).await;
        {{/if}}
    }
    {{else}}
    for (idx, item) in source.iter().enumerate() {
        state.set("{{forEach.itemVar}}", item.clone());
        state.set("{{forEach.indexVar}}", idx);
        
        let result = executor.execute_subgraph("{{loopBody}}", state).await?;
        {{#if results.collect}}
        results.push(result);
        {{/if}}
    }
    {{/if}}
    
    {{#if results.collect}}
    state.set("{{results.aggregationKey}}", serde_json::json!(results));
    {{/if}}
    
    Ok(results)
}
```

### Error Handling Wrapper

All action nodes are wrapped with standard error handling:

```rust
async fn execute_action_node<F, T>(
    node_id: &str,
    config: &StandardProperties,
    state: &mut State,
    action: F,
) -> Result<T, ActionError>
where
    F: Fn(&mut State) -> Pin<Box<dyn Future<Output = Result<T, ActionError>> + Send>>,
{
    // Check skip condition
    if let Some(condition) = &config.execution.condition {
        if !evaluate_condition(condition, state)? {
            tracing::info!(node_id, "Skipping node due to condition");
            return Err(ActionError::Skipped);
        }
    }
    
    // Execute callbacks
    if let Some(on_start) = &config.callbacks.on_start {
        execute_callback(on_start, state).await?;
    }
    
    // Execute with timeout and retry logic
    let result = match config.error_handling.mode {
        ErrorMode::Stop => {
            tokio::time::timeout(
                Duration::from_millis(config.execution.timeout),
                action(state)
            ).await??
        }
        ErrorMode::Retry => {
            let mut attempts = 0;
            loop {
                match tokio::time::timeout(
                    Duration::from_millis(config.execution.timeout),
                    action(state)
                ).await {
                    Ok(Ok(result)) => break result,
                    Ok(Err(e)) | Err(e) => {
                        attempts += 1;
                        if attempts >= config.error_handling.retry_count.unwrap_or(3) {
                            return Err(e.into());
                        }
                        tokio::time::sleep(Duration::from_millis(
                            config.error_handling.retry_delay.unwrap_or(1000)
                        )).await;
                    }
                }
            }
        }
        ErrorMode::Continue => {
            match tokio::time::timeout(
                Duration::from_millis(config.execution.timeout),
                action(state)
            ).await {
                Ok(Ok(result)) => result,
                Ok(Err(e)) | Err(e) => {
                    tracing::warn!(node_id, error = ?e, "Node failed, continuing");
                    if let Some(on_error) = &config.callbacks.on_error {
                        execute_callback(on_error, state).await?;
                    }
                    return Err(ActionError::ContinuedAfterError(e.to_string()));
                }
            }
        }
        ErrorMode::Fallback => {
            match tokio::time::timeout(
                Duration::from_millis(config.execution.timeout),
                action(state)
            ).await {
                Ok(Ok(result)) => result,
                Ok(Err(_)) | Err(_) => {
                    let fallback = config.error_handling.fallback_value.clone()
                        .ok_or(ActionError::NoFallbackValue)?;
                    state.set(&config.mapping.output_key, fallback.clone());
                    return Ok(fallback);
                }
            }
        }
    };
    
    // Execute completion callback
    if let Some(on_complete) = &config.callbacks.on_complete {
        execute_callback(on_complete, state).await?;
    }
    
    Ok(result)
}
```

## Data Models

### Project Schema Extension

```typescript
// Extended project schema with action nodes
export interface Project {
  id: string;
  name: string;
  // ... existing fields
  
  // Existing
  agents: Record<string, AgentSchema>;
  
  // New: Action nodes
  actionNodes: Record<string, ActionNodeConfig>;
  
  workflow: {
    edges: Array<{
      from: string;
      to: string;
      fromPort?: string;  // For multi-output nodes like Switch
      toPort?: string;
    }>;
  };
}
```

### SSE Event Extension

```typescript
// Extended SSE events for action nodes
export interface ActionNodeEvent {
  type: 'action_start' | 'action_end' | 'action_error';
  nodeId: string;
  nodeType: ActionNodeConfig['type'];
  timestamp: number;
  
  // State snapshot
  state_snapshot?: {
    input: Record<string, unknown>;
    output: Record<string, unknown>;
  };
  
  // Error details
  error?: {
    message: string;
    code: string;
    retryAttempt?: number;
  };
  
  // Loop-specific
  iteration?: {
    current: number;
    total: number;
  };
}
```

## Correctness Properties

Based on the acceptance criteria, the following correctness properties are identified:

### Property 1: Standard Properties Persistence

*For any* action node configuration, saving and loading the project SHALL preserve all standard properties (error handling, tracing, callbacks, execution control, mapping).

**Validates: Requirements 1.1-1.6**

### Property 2: Error Handling Mode Behavior

*For any* action node with error mode set to `retry`, the node SHALL retry up to `retryCount` times with `retryDelay` between attempts before failing.

**Validates: Requirements 1.2**

### Property 3: HTTP Variable Interpolation

*For any* HTTP node URL containing `{{variable}}` patterns, the system SHALL replace all patterns with corresponding state values before making the request.

**Validates: Requirements 3.1**

### Property 4: Switch Condition Evaluation

*For any* Switch node in `first_match` mode, the system SHALL evaluate conditions in order and route to the first matching branch.

**Validates: Requirements 6.1, 6.2**

### Property 5: Loop Result Aggregation

*For any* Loop node with `collect: true`, the system SHALL aggregate all iteration results into an array at the specified `aggregationKey`.

**Validates: Requirements 7.4**

### Property 6: Merge Wait Behavior

*For any* Merge node in `wait_all` mode, the node SHALL not proceed until all incoming branches have completed.

**Validates: Requirements 8.1**

### Property 7: Code Sandbox Isolation

*For any* Code node with `networkAccess: false`, the executed code SHALL NOT be able to make network requests.

**Validates: Requirements 10.2**

### Property 8: Database Connection Security

*For any* Database node, the connection string SHALL be stored as a secret and masked in logs.

**Validates: Requirements 11.2**

### Property 9: Action Node Visual Distinction

*For any* action node on the canvas, the node SHALL display with a distinct color and icon matching its type.

**Validates: Requirements 12.1**

### Property 10: Code Generation Validity

*For any* workflow containing action nodes, the generated Rust code SHALL compile without errors.

**Validates: Requirements 13.1**

## Use Case: Excel Update Workflow

This use case validates the action node design by implementing a real-world workflow:

**Scenario**: Update an Excel sheet based on email sentiment analysis

```
┌─────────┐    ┌─────────┐    ┌─────────┐    ┌─────────┐
│ Trigger │───▶│  HTTP   │───▶│Transform│───▶│  Loop   │
│ (cron)  │    │ (Gmail) │    │ (parse) │    │(forEach)│
└─────────┘    └─────────┘    └─────────┘    └────┬────┘
                                                   │
                                                   ▼
                                            ┌─────────┐
                                            │LLM Agent│
                                            │(analyze)│
                                            └────┬────┘
                                                 │
                                                 ▼
                                            ┌─────────┐
                                            │ Switch  │
                                            │(routing)│
                                            └────┬────┘
                                    ┌────────────┼────────────┐
                                    ▼            ▼            ▼
                              ┌─────────┐  ┌─────────┐  ┌─────────┐
                              │   Set   │  │   Set   │  │   Set   │
                              │(positive│  │(neutral)│  │(negative│
                              └────┬────┘  └────┬────┘  └────┬────┘
                                   │            │            │
                                   └────────────┼────────────┘
                                                ▼
                                          ┌─────────┐
                                          │  Merge  │
                                          │(wait_all│
                                          └────┬────┘
                                               │
                                               ▼
                                          ┌─────────┐
                                          │  HTTP   │
                                          │(Sheets) │
                                          └─────────┘
```

### Node Configurations

```typescript
// 1. Trigger Node
const trigger: TriggerNodeConfig = {
  type: 'trigger',
  triggerType: 'schedule',
  schedule: {
    cron: '0 9 * * *',  // Daily at 9 AM
    timezone: 'America/New_York'
  },
  // Standard properties...
};

// 2. HTTP Node (Gmail)
const gmailFetch: HttpNodeConfig = {
  type: 'http',
  method: 'GET',
  url: 'https://gmail.googleapis.com/gmail/v1/users/me/messages?q=is:unread',
  auth: {
    type: 'bearer',
    bearer: { token: '{{GMAIL_TOKEN}}' }
  },
  response: {
    type: 'json',
    jsonPath: '$.messages'
  },
  mapping: {
    outputKey: 'emails'
  },
  // Standard properties...
};

// 3. Transform Node
const parseEmails: TransformNodeConfig = {
  type: 'transform',
  transformType: 'javascript',
  expression: `
    return input.emails.map(email => ({
      id: email.id,
      subject: email.payload.headers.find(h => h.name === 'Subject')?.value,
      body: email.snippet
    }));
  `,
  mapping: {
    outputKey: 'parsedEmails'
  },
  // Standard properties...
};

// 4. Loop Node
const processLoop: LoopNodeConfig = {
  type: 'loop',
  loopType: 'forEach',
  forEach: {
    sourceArray: 'parsedEmails',
    itemVar: 'email',
    indexVar: 'idx'
  },
  parallel: {
    enabled: true,
    batchSize: 5
  },
  results: {
    collect: true,
    aggregationKey: 'analysisResults'
  },
  // Standard properties...
};

// 5. LLM Agent (existing ADK agent)
// Analyzes email sentiment

// 6. Switch Node
const sentimentRouter: SwitchNodeConfig = {
  type: 'switch',
  evaluationMode: 'first_match',
  conditions: [
    { id: 'pos', name: 'Positive', field: 'sentiment.score', operator: 'gt', value: 0.5, outputPort: 'positive' },
    { id: 'neg', name: 'Negative', field: 'sentiment.score', operator: 'lt', value: -0.5, outputPort: 'negative' },
  ],
  defaultBranch: 'neutral',
  // Standard properties...
};

// 7. Set Nodes (one per branch)
const setPositive: SetNodeConfig = {
  type: 'set',
  mode: 'set',
  variables: [
    { key: 'category', value: 'Positive', valueType: 'string', isSecret: false },
    { key: 'color', value: '#22C55E', valueType: 'string', isSecret: false }
  ],
  // Standard properties...
};

// 8. Merge Node
const mergeBranches: MergeNodeConfig = {
  type: 'merge',
  mode: 'wait_all',
  combineStrategy: 'array',
  timeout: { enabled: true, ms: 30000, behavior: 'continue' },
  // Standard properties...
};

// 9. HTTP Node (Google Sheets)
const updateSheet: HttpNodeConfig = {
  type: 'http',
  method: 'POST',
  url: 'https://sheets.googleapis.com/v4/spreadsheets/{{SHEET_ID}}/values/A1:append',
  auth: {
    type: 'bearer',
    bearer: { token: '{{SHEETS_TOKEN}}' }
  },
  body: {
    type: 'json',
    content: {
      values: '{{analysisResults}}'
    }
  },
  // Standard properties...
};
```

This use case demonstrates:
- All 10 action node types working together
- Integration with LLM agents
- Parallel processing with Loop
- Conditional routing with Switch
- Branch merging with Merge
- External API integration with HTTP
- State management with Set and Transform


## Additional Node Types (Gap Analysis)

Based on n8n workflow analysis, the following additional nodes are required:

### Email Node

```typescript
// types/actionNodes.ts (addition)

export interface EmailNodeConfig extends StandardProperties {
  type: 'email';
  mode: 'monitor' | 'send';
  
  // IMAP monitoring config
  imap?: {
    host: string;
    port: number;
    secure: boolean;
    username: string;
    password: string;  // Secret
    folder: string;
    filter?: {
      from?: string;
      subject?: string;
      since?: string;
    };
    markAsRead: boolean;
  };
  
  // SMTP sending config
  smtp?: {
    host: string;
    port: number;
    secure: boolean;
    username: string;
    password: string;  // Secret
  };
  
  // Email content (for send mode)
  email?: {
    to: string[];
    cc?: string[];
    bcc?: string[];
    subject: string;
    body: string;
    bodyType: 'text' | 'html';
    attachments?: string[];  // State keys containing file data
  };
}
```

### RSS/Feed Node

```typescript
export interface RssNodeConfig extends StandardProperties {
  type: 'rss';
  feedUrl: string;
  pollInterval: number;  // seconds
  
  filter?: {
    keywords?: string[];
    since?: string;
  };
  
  trackSeen: boolean;
  seenStorageKey?: string;
  
  output: {
    maxItems: number;
    includeContent: boolean;
  };
}
```

### File Node

```typescript
export interface FileNodeConfig extends StandardProperties {
  type: 'file';
  operation: 'read' | 'write' | 'delete' | 'list';
  
  // Local file config
  local?: {
    path: string;
    encoding?: string;
  };
  
  // Cloud storage config
  cloud?: {
    provider: 's3' | 'gcs' | 'azure';
    bucket: string;
    key: string;
    credentials: string;  // Reference to Set node
  };
  
  // Parsing options (for read)
  parse?: {
    format: 'json' | 'csv' | 'xml' | 'text';
    csvOptions?: {
      delimiter: string;
      hasHeader: boolean;
    };
  };
  
  // Write options
  write?: {
    content: string;  // State key or expression
    createDirs: boolean;
  };
}
```

### Notification Node

```typescript
export interface NotificationNodeConfig extends StandardProperties {
  type: 'notification';
  channel: 'slack' | 'discord' | 'teams' | 'webhook';
  
  webhookUrl: string;  // Secret
  
  message: {
    text: string;
    format: 'plain' | 'markdown' | 'blocks';
    blocks?: unknown[];  // Slack Block Kit / Discord Embeds
  };
  
  // Optional fields
  username?: string;
  iconUrl?: string;
  channel?: string;  // For Slack
}
```

### Vector Search Node (Stretch)

```typescript
export interface VectorSearchNodeConfig extends StandardProperties {
  type: 'vectorSearch';
  provider: 'pinecone' | 'weaviate' | 'qdrant' | 'chroma';
  
  connection: {
    apiKey: string;  // Secret
    environment?: string;
    indexName: string;
  };
  
  operation: 'upsert' | 'search' | 'delete';
  
  // Upsert config
  upsert?: {
    vectors: string;  // State key containing vectors
    metadata?: Record<string, unknown>;
  };
  
  // Search config
  search?: {
    query: string;  // Text to embed and search
    topK: number;
    filter?: Record<string, unknown>;
    includeMetadata: boolean;
  };
  
  // Delete config
  delete?: {
    ids?: string[];
    filter?: Record<string, unknown>;
  };
}
```

### Document Parser Node (Stretch)

```typescript
export interface DocumentParserNodeConfig extends StandardProperties {
  type: 'documentParser';
  source: 'state' | 'url' | 'path';
  sourceKey: string;
  
  documentType: 'pdf' | 'docx' | 'xlsx' | 'image';
  
  extraction: {
    mode: 'full' | 'pages' | 'tables';
    pages?: number[];  // For PDF
    sheet?: string;    // For Excel
  };
  
  // OCR config (for images/scanned PDFs)
  ocr?: {
    enabled: boolean;
    provider: 'tesseract' | 'google_vision' | 'aws_textract';
    apiKey?: string;
  };
}
```

### Updated Visual Constants

```typescript
// Extended color palette for new nodes
export const ACTION_NODE_COLORS = {
  // Original 10
  trigger: '#6366F1',    // Indigo
  http: '#3B82F6',       // Blue
  set: '#8B5CF6',        // Purple
  transform: '#EC4899',  // Pink
  switch: '#F59E0B',     // Amber
  loop: '#10B981',       // Emerald
  merge: '#06B6D4',      // Cyan
  wait: '#6B7280',       // Gray
  code: '#EF4444',       // Red
  database: '#14B8A6',   // Teal
  
  // New nodes
  email: '#F97316',      // Orange
  rss: '#84CC16',        // Lime
  file: '#A855F7',       // Violet
  notification: '#22D3EE', // Sky
  vectorSearch: '#F472B6', // Pink (stretch)
  documentParser: '#FB923C', // Orange (stretch)
};

export const ACTION_NODE_ICONS = {
  // Original 10
  trigger: '🎯',
  http: '🌐',
  set: '📝',
  transform: '⚙️',
  switch: '🔀',
  loop: '🔄',
  merge: '🔗',
  wait: '⏱️',
  code: '💻',
  database: '🗄️',
  
  // New nodes
  email: '📧',
  rss: '📡',
  file: '📁',
  notification: '🔔',
  vectorSearch: '🔍',
  documentParser: '📄',
};
```

## n8n Workflow Mapping

Here's how each n8n workflow maps to our action nodes:

### Workflow 1: AI-Powered Lead Generation

```
n8n Node              → ADK Studio Node
─────────────────────────────────────────
Webhook/Trigger       → Trigger (webhook)
OpenAI/Claude         → LLM Agent
AI Agent              → LLM Agent
HTTP Request          → HTTP
Router/IF             → Switch
CRM (HubSpot)         → HTTP (API call)
Slack/Email           → Notification / Email
```

### Workflow 4: AI Customer Support

```
n8n Node              → ADK Studio Node
─────────────────────────────────────────
Webhook               → Trigger (webhook)
OpenAI/Claude         → LLM Agent (classify)
Semantic Search AI    → Vector Search + LLM Agent
Vector Database       → Vector Search
RAG                   → LLM Agent (with context)
AI Confidence Checker → LLM Agent (score)
Router/IF             → Switch
Email/Slack           → Email / Notification
Sentiment AI          → LLM Agent
Help Desk System      → HTTP (API call)
```

### Workflow 6: AI Invoice Processing

```
n8n Node              → ADK Studio Node
─────────────────────────────────────────
Email/IMAP            → Email (monitor)
OCR + AI              → Document Parser + LLM Agent
OpenAI                → LLM Agent
AI Fraud Detection    → LLM Agent
Database              → Database
AI Approval Router    → Switch
QuickBooks/Xero       → HTTP (API call)
Email                 → Email (send)
Google Sheets         → HTTP (Sheets API)
```

### Workflow 10: AI-Powered Incident Response

```
n8n Node              → ADK Studio Node
─────────────────────────────────────────
Webhook/Monitor       → Trigger (webhook)
AI Log Analyzer       → LLM Agent
AI Root Cause         → LLM Agent
Vector Search         → Vector Search
OpenAI/Claude         → LLM Agent
AI Severity Classifier→ LLM Agent + Switch
PagerDuty/OpsGenie    → HTTP (API call)
AI Runbook Generator  → LLM Agent
Slack                 → Notification
AI Communication      → LLM Agent
Status Page           → HTTP (API call)
Post-Mortem AI        → LLM Agent
```

## Implementation Priority

### Phase 1: Core (10 nodes) - 4-6 weeks
1. Trigger, HTTP, Set, Transform, Switch
2. Loop, Merge, Wait, Code, Database

### Phase 2: Communication (2 nodes) - 2 weeks
3. Email, Notification

### Phase 3: Data Sources (2 nodes) - 2 weeks
4. RSS/Feed, File

### Phase 4: AI Enhancement (Stretch) - 3-4 weeks
5. Vector Search, Document Parser

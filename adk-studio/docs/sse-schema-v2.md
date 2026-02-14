# SSE Schema v2.0

This document describes the Server-Sent Events (SSE) schema for ADK Studio v2.0, which adds support for state snapshots and data flow overlays.

## Overview

SSE Schema v2 extends the existing event format to include:
- **State snapshots**: Input/output state at each agent execution step
- **State keys**: List of state keys for data flow overlay visualization

These additions enable:
- Timeline debugging with state inspection
- Data flow overlays showing which state keys flow between nodes
- Real-time state inspection during execution

## Event Types

### Core Events

| Event Type | Description |
|------------|-------------|
| `session` | Session ID for the current execution |
| `chunk` | Streaming text response chunk |
| `trace` | Execution trace events (node_start, node_end, state) |
| `tool_call` | Tool invocation event |
| `tool_result` | Tool execution result |
| `log` | General log message |
| `end` | Execution complete |
| `error` | Error occurred |

### v2.0 Enhanced Trace Events

The `trace` event payload has been enhanced with state snapshot support:

```typescript
interface TraceEvent {
  type: 'node_start' | 'node_end' | 'state' | 'done';
  node?: string;           // Agent/node name
  step?: number;           // Execution step number
  duration_ms?: number;    // Duration for node_end events
  
  // v2.0: State snapshot fields
  state_snapshot?: StateSnapshot;
  state_keys?: string[];   // Top-level keys in output state
}

interface StateSnapshot {
  input: Record<string, unknown>;   // Input state before execution
  output: Record<string, unknown>;  // Output state after execution
}
```

## Event Payloads

### node_start Event

Emitted when an agent begins execution.

```json
{
  "type": "node_start",
  "node": "researcher",
  "step": 1,
  "state_snapshot": {
    "input": {
      "query": "What is ADK?",
      "context": []
    },
    "output": {}
  },
  "state_keys": ["query", "context"]
}
```

### node_end Event

Emitted when an agent completes execution.

```json
{
  "type": "node_end",
  "node": "researcher",
  "step": 1,
  "duration_ms": 1234,
  "state_snapshot": {
    "input": {
      "query": "What is ADK?",
      "context": []
    },
    "output": {
      "query": "What is ADK?",
      "context": [],
      "research_results": "ADK is..."
    }
  },
  "state_keys": ["query", "context", "research_results"]
}
```

### state Event

Emitted for intermediate state updates during execution.

```json
{
  "type": "state",
  "state_snapshot": {
    "input": {},
    "output": {
      "response": "Processing..."
    }
  },
  "state_keys": ["response"]
}
```

### done Event

Emitted when the entire workflow completes.

```json
{
  "type": "done",
  "total_steps": 5,
  "state_snapshot": {
    "input": {
      "query": "What is ADK?"
    },
    "output": {
      "query": "What is ADK?",
      "response": "ADK is the Agent Development Kit..."
    }
  },
  "state_keys": ["query", "response"]
}
```

## State Snapshot Capture Rules

1. **Capture at agent_start**: Capture input state when an agent begins execution
2. **Capture at agent_end**: Capture both input and output state when an agent completes
3. **Extract state_keys**: Extract top-level keys from the output state for data flow overlays
4. **Limit nested depth**: Keep payload size reasonable by limiting nested object depth to 3 levels
5. **Best-effort retention**: Frontend retains up to 100 snapshots (oldest are dropped)

## Frontend Integration

### useSSE Hook Updates

The `useSSE` hook parses the new payload fields:

```typescript
interface StateSnapshot {
  nodeId: string;
  timestamp: number;
  inputState: Record<string, unknown>;
  outputState: Record<string, unknown>;
  duration: number;
  status: 'running' | 'success' | 'error';
  error?: string;
}

// New fields in useSSE return value
interface UseSSEReturn {
  // ... existing fields
  snapshots: StateSnapshot[];      // State snapshots for timeline
  currentSnapshotIndex: number;    // Current position in timeline
  stateKeys: Map<string, string[]>; // Edge ID -> state keys mapping
}
```

### Data Flow Overlay Integration

State keys from SSE events are mapped to edges:

```typescript
// When node_end event is received:
// 1. Extract state_keys from event
// 2. Find outgoing edges from the completed node
// 3. Update edge data with state_keys for overlay display
```

## Backend Implementation

### Rust Types

```rust
/// State snapshot for timeline/inspector
#[derive(Debug, Clone, Serialize)]
pub struct StateSnapshot {
    pub input: serde_json::Value,
    pub output: serde_json::Value,
}

/// Enhanced trace event for v2.0
#[derive(Debug, Serialize)]
pub struct TraceEventV2 {
    #[serde(rename = "type")]
    pub event_type: String,
    
    #[serde(skip_serializing_if = "Option::is_none")]
    pub node: Option<String>,
    
    #[serde(skip_serializing_if = "Option::is_none")]
    pub step: Option<u32>,
    
    #[serde(skip_serializing_if = "Option::is_none")]
    pub duration_ms: Option<u64>,
    
    #[serde(skip_serializing_if = "Option::is_none")]
    pub total_steps: Option<u32>,
    
    // v2.0: State snapshot fields
    #[serde(skip_serializing_if = "Option::is_none")]
    pub state_snapshot: Option<StateSnapshot>,
    
    #[serde(skip_serializing_if = "Option::is_none")]
    pub state_keys: Option<Vec<String>>,
}
```

### Event Emission Points

1. **Agent Start**: Emit `node_start` with input state snapshot
2. **Agent End**: Emit `node_end` with full state snapshot and state_keys
3. **Workflow Complete**: Emit `done` with final state snapshot

## Migration Notes

### Backward Compatibility

- v2.0 events are backward compatible with v1.0 consumers
- New fields (`state_snapshot`, `state_keys`) are optional
- Existing event types and fields remain unchanged

### Frontend Migration

1. Update `useSSE` hook to parse new fields
2. Add `snapshots` state array for timeline
3. Add `stateKeys` map for edge data
4. Update execution state types

## Example Full Event Stream

```
event: session
data: abc123

event: trace
data: {"type":"node_start","node":"researcher","step":1,"state_snapshot":{"input":{"query":"test"},"output":{}},"state_keys":["query"]}

event: log
data: {"message":"Calling gemini-2.5-flash (tools: 2)"}

event: tool_call
data: {"name":"search","args":{"query":"test"}}

event: tool_result
data: {"name":"search","result":"..."}

event: chunk
data: Based on my research...

event: trace
data: {"type":"node_end","node":"researcher","step":1,"duration_ms":1500,"state_snapshot":{"input":{"query":"test"},"output":{"query":"test","results":"..."}},"state_keys":["query","results"]}

event: trace
data: {"type":"done","total_steps":1,"state_snapshot":{"input":{"query":"test"},"output":{"query":"test","results":"...","response":"..."}},"state_keys":["query","results","response"]}

event: end
data: 
```

## Requirements Traceability

- **Requirement 5.8**: State snapshot capture at each node during execution
- **Requirement 3.3**: State keys sourced from runtime execution events

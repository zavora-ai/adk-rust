# Examples Parity Analysis

## Go ADK Examples Structure

```
examples/
├── quickstart/          # Basic weather agent with GoogleSearch
├── rest/                # REST API server
├── a2a/                 # A2A protocol with remote agent
├── mcp/                 # MCP integration
├── web/                 # Multi-agent web app with artifacts
├── tools/
│   ├── multipletools/   # Multiple tools composition
│   └── loadartifacts/   # Load artifacts tool
├── workflowagents/
│   ├── sequential/      # Sequential workflow
│   ├── sequentialCode/  # Sequential with code generation
│   ├── parallel/        # Parallel workflow
│   └── loop/            # Loop workflow
└── vertexai/
    └── imagegenerator/  # Vertex AI Imagen integration
```

## Current Rust Examples (Flat Structure)

```
examples/
├── quickstart.rs        # ✅ Weather agent
├── server.rs            # ✅ REST server
├── function_tool.rs     # ✅ Custom function tool
├── multiple_tools.rs    # ✅ Multiple tools
├── sequential.rs        # ✅ Sequential workflow
├── parallel.rs          # ✅ Parallel workflow
├── loop_workflow.rs     # ✅ Loop workflow
└── load_artifacts.rs    # ✅ Load artifacts
```

## Parity Status

| Go Example | Rust Status | Notes |
|------------|-------------|-------|
| quickstart | ✅ Complete | `quickstart.rs` |
| rest | ✅ Complete | `server.rs` |
| a2a | ❌ Missing | Need remote agent example |
| mcp | ⚠️ Partial | McpToolset implemented, need example |
| web | ❌ Missing | Multi-agent with artifacts |
| tools/multipletools | ✅ Complete | `multiple_tools.rs` |
| tools/loadartifacts | ✅ Complete | `load_artifacts.rs` |
| workflowagents/sequential | ✅ Complete | `sequential.rs` |
| workflowagents/sequentialCode | ❌ Missing | Code generation workflow |
| workflowagents/parallel | ✅ Complete | `parallel.rs` |
| workflowagents/loop | ✅ Complete | `loop_workflow.rs` |
| vertexai/imagegenerator | ❌ Missing | Imagen integration |
| - | ✅ Extra | `function_tool.rs` (good to have) |

## Missing Examples (Priority Order)

### 1. MCP Example (High Priority)
- **File**: `examples/mcp/main.rs`
- **Purpose**: Demonstrate McpToolset integration
- **Features**: Connect to MCP server, use MCP tools

### 2. A2A Remote Agent (High Priority)
- **File**: `examples/a2a/main.rs`
- **Purpose**: Demonstrate A2A protocol with remote agent
- **Features**: Start A2A server, connect via remote agent

### 3. Web Multi-Agent (Medium Priority)
- **File**: `examples/web/main.rs`
- **Purpose**: Multi-agent app with artifact callbacks
- **Features**: Multiple agents, artifact saving, agent composition

### 4. Sequential Code Generation (Medium Priority)
- **File**: `examples/workflowagents/sequential_code.rs`
- **Purpose**: Code generation workflow
- **Features**: Sequential agents for code tasks

### 5. Vertex AI Imagen (Low Priority)
- **File**: `examples/vertexai/image_generator.rs`
- **Purpose**: Image generation with Vertex AI
- **Features**: Imagen model, artifact storage, local file saving
- **Note**: Requires Vertex AI setup, not just API key

## Proposed Rust Structure

### Option 1: Organized by Category (Recommended)
```
examples/
├── basic/
│   ├── quickstart.rs
│   ├── function_tool.rs
│   └── multiple_tools.rs
├── servers/
│   ├── rest.rs
│   └── a2a.rs
├── workflows/
│   ├── sequential.rs
│   ├── sequential_code.rs
│   ├── parallel.rs
│   └── loop.rs
├── tools/
│   ├── load_artifacts.rs
│   └── mcp.rs
└── advanced/
    ├── web_multiagent.rs
    └── image_generator.rs
```

### Option 2: Keep Flat (Current)
- Simpler for users
- Easier to discover
- Less navigation
- **Recommended for now** (8-12 examples is manageable)

## Implementation Plan

1. **Add MCP example** (~30 lines)
   - Connect to MCP server
   - Use McpToolset
   - Demonstrate tool discovery

2. **Add A2A example** (~60 lines)
   - Start embedded A2A server
   - Create remote agent
   - Demonstrate agent-to-agent communication

3. **Add web multi-agent example** (~80 lines)
   - Multiple specialized agents
   - Artifact callbacks
   - Agent composition

4. **Add sequential code example** (~50 lines)
   - Code generation workflow
   - Multi-step refinement

5. **Defer Imagen example**
   - Requires Vertex AI setup
   - Not essential for core functionality
   - Can be added later

## Total: 4 New Examples (~220 lines)

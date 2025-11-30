# Long Running Function Tools

> **Status**: ✅ Implemented
> **Priority**: Medium
> **Completed**: 2025

## Overview

Long Running Function Tools enable agents to work with operations that take significant time to complete, such as data processing jobs, file conversions, or external API calls that return asynchronously. Unlike regular function tools that block until completion, long-running tools return immediately with a status indicator and allow the agent to check progress or retrieve results later.

## The Problem

Standard function tools in ADK-Rust execute synchronously - the agent calls the tool and waits for the complete result before continuing. This works well for quick operations, but creates issues for time-consuming tasks:

- **Blocking**: The agent is stuck waiting, unable to handle other requests
- **Timeouts**: Long operations may exceed model timeout limits
- **Poor UX**: Users see no progress updates during lengthy operations
- **Resource Waste**: Holding connections open for extended periods

## Planned Solution

Long Running Function Tools will introduce a pattern where:

1. **Initial Call**: Tool returns immediately with a task ID and status
2. **Status Tracking**: Agent can check progress without blocking
3. **Result Retrieval**: Agent fetches final results when ready
4. **Intermediate Updates**: Tool can provide progress information

## Planned Architecture

### Tool Configuration

```rust,ignore
use adk_tool::FunctionTool;

// Create a long-running function tool
let process_video = FunctionTool::builder()
    .name("process_video")
    .description("Process a video file with effects")
    .is_long_running(true)  // Mark as long-running
    .handler(|ctx, args: ProcessVideoArgs| async move {
        // Start processing asynchronously
        let task_id = start_video_processing(args).await?;
        
        // Return immediately with task ID
        Ok(json!({
            "status": "processing",
            "task_id": task_id,
            "progress": 0
        }))
    })
    .build()?;
```

### Agent Behavior

When an agent calls a long-running tool:

1. **First Response**: Tool returns task ID and initial status
2. **Agent Awareness**: Agent knows not to call the tool again immediately
3. **Status Checks**: Agent can query status using the task ID
4. **Completion**: Agent retrieves final results when status is "completed"

### Event Tracking

The session event system will track long-running operations:

```rust,ignore
pub struct Event {
    pub id: String,
    pub timestamp: DateTime<Utc>,
    pub author: String,
    pub content: Option<Content>,
    pub actions: EventActions,
    
    // Track long-running tool calls
    pub long_running_tool_ids: Vec<String>,
}
```

### Tool Declaration Enhancement

Long-running tools will include special instructions in their declarations:

```rust,ignore
impl FunctionTool {
    fn declaration(&self) -> FunctionDeclaration {
        let mut decl = FunctionDeclaration {
            name: self.name.clone(),
            description: self.description.clone(),
            parameters: self.parameters.clone(),
        };
        
        if self.is_long_running {
            let note = "NOTE: This is a long-running operation. \
                       Do not call this tool again if it has already \
                       returned some intermediate or pending status.";
            decl.description = format!("{}\n\n{}", decl.description, note);
        }
        
        decl
    }
}
```

## Use Cases

### Video Processing

```rust,ignore
// Agent starts video processing
let result = agent.run("Process video.mp4 with blur effect").await?;
// Returns: "I've started processing your video. Task ID: task_123"

// Agent can check status
let status = agent.run("What's the status of task_123?").await?;
// Returns: "Processing is 45% complete"

// Agent retrieves final result
let final_result = agent.run("Get results for task_123").await?;
// Returns: "Video processing complete! Output: processed_video.mp4"
```

### Data Analysis

```rust,ignore
// Long-running data analysis
let analyze_tool = FunctionTool::builder()
    .name("analyze_dataset")
    .description("Analyze large dataset and generate report")
    .is_long_running(true)
    .handler(|ctx, args: AnalyzeArgs| async move {
        let job_id = submit_analysis_job(args.dataset_path).await?;
        Ok(json!({
            "status": "queued",
            "job_id": job_id,
            "estimated_time": "5 minutes"
        }))
    })
    .build()?;
```

### External API Integration

```rust,ignore
// Call external API that processes asynchronously
let external_api_tool = FunctionTool::builder()
    .name("generate_report")
    .description("Generate PDF report via external service")
    .is_long_running(true)
    .handler(|ctx, args: ReportArgs| async move {
        let response = external_api.submit_job(args).await?;
        Ok(json!({
            "status": "pending",
            "request_id": response.id,
            "check_url": response.status_url
        }))
    })
    .build()?;
```

## Implementation Status

### Phase 1: Core Infrastructure ✅
- [x] Add `is_long_running` field to `FunctionTool` (`adk-tool/src/function_tool.rs`)
- [x] Add `long_running_tool_ids` to `Event` struct (`adk-core/src/event.rs`)
- [x] Implement tool declaration enhancement with NOTE via `enhanced_description()`
- [x] Add `is_long_running()` to `Tool` trait (`adk-core/src/tool.rs`)

### Phase 2: Agent Integration ✅
- [x] LlmAgent populates `long_running_tool_ids` on function call events
- [x] Agent breaks loop after executing all long-running tools (treats as final response)
- [x] `Event::is_final_response()` checks `long_running_tool_ids`
- [x] `enhanced_description()` warns model not to call long-running tools again

### Phase 3: Testing ✅
- [x] Unit tests for `FunctionTool.is_long_running()`
- [x] Unit tests for `FunctionTool.enhanced_description()`
- [x] Unit tests for `Event.is_final_response()` with long-running tools
- [x] Unit tests for `Event.function_call_ids()`

### Future Enhancements
- [ ] Create example: video processing simulation
- [ ] Create example: data analysis job
- [ ] Create example: external API integration
- [ ] Add `CodeExecutionResult` variant to `Part` enum

## API Design

### FunctionTool Builder

```rust,ignore
pub struct FunctionToolBuilder {
    name: String,
    description: String,
    is_long_running: bool,
    // ... other fields
}

impl FunctionToolBuilder {
    pub fn is_long_running(mut self, value: bool) -> Self {
        self.is_long_running = value;
        self
    }
}
```

### Tool Trait Extension

```rust,ignore
pub trait Tool: Send + Sync {
    fn name(&self) -> &str;
    fn description(&self) -> &str;
    
    // New method for long-running detection
    fn is_long_running(&self) -> bool {
        false  // Default implementation
    }
}
```

### Status Response Format

Long-running tools should return responses in a consistent format:

```rust,ignore
// Initial response
{
    "status": "processing" | "queued" | "pending",
    "task_id": "unique_identifier",
    "progress": 0-100,  // Optional
    "estimated_time": "5 minutes"  // Optional
}

// Progress update
{
    "status": "processing",
    "task_id": "unique_identifier",
    "progress": 45,
    "message": "Processing frame 450/1000"
}

// Completion response
{
    "status": "completed",
    "task_id": "unique_identifier",
    "result": { /* actual result data */ }
}

// Error response
{
    "status": "failed",
    "task_id": "unique_identifier",
    "error": "Error message"
}
```

## Comparison with adk-go

ADK-Go has full long-running tool support with:
- `IsLongRunning` field on function tools
- Automatic tracking of long-running tool IDs in events
- Agent awareness to prevent duplicate calls
- Integration with A2A protocol for task status updates

ADK-Rust will achieve feature parity with these capabilities.

## Related Features

### A2A Protocol Integration

Long-running tools integrate with the Agent-to-Agent (A2A) protocol:
- Task status updates via A2A events
- `TaskStateInputRequired` for operations awaiting completion
- Artifact updates for intermediate results

### Session State

Long-running operations can store state:
```rust,ignore
// Store task ID in session state
ctx.state().set("current_task_id", task_id)?;

// Retrieve later
let task_id = ctx.state().get("current_task_id")?;
```

## Best Practices

### Tool Design

1. **Return Immediately**: Don't block in the handler
2. **Provide Task IDs**: Always return a unique identifier
3. **Include Status**: Use consistent status values
4. **Estimate Time**: Help users understand wait times
5. **Support Cancellation**: Allow operations to be cancelled

### Agent Instructions

Guide agents on handling long-running operations:

```rust,ignore
let agent = LlmAgentBuilder::new("assistant")
    .instruction(
        "When you call a long-running tool, it will return a task ID. \
         Wait for the user to ask about progress before checking status. \
         Don't repeatedly call the same long-running operation."
    )
    .build()?;
```

### Error Handling

```rust,ignore
// Handle failures gracefully
match tool_response.status {
    "completed" => process_result(tool_response.result),
    "failed" => handle_error(tool_response.error),
    "processing" => inform_user_of_progress(tool_response.progress),
    _ => return Err("Unknown status"),
}
```

## Implementation Complete

Long-running tool support has been implemented following the design patterns established in ADK-Go while leveraging Rust's async capabilities and type safety.

Key achievements:
1. ✅ Core infrastructure: `is_long_running` flag, `long_running_tool_ids` tracking
2. ✅ Agent flow integration: LlmAgent populates IDs and handles final responses correctly
3. ✅ Enhanced descriptions: Model warned not to call long-running tools again
4. ✅ Comprehensive unit tests for all new functionality

## Contributing

If you're interested in contributing to long-running tool support in ADK-Rust, please:

1. Review the existing code in `adk-tool/src/function_tool.rs`
2. Familiarize yourself with the ADK-Go implementation
3. Check the session event system in `adk-session/`
4. Open an issue to discuss your approach

---

**Related**:
- [Function Tools Documentation](../official_docs/tools/function-tools.md)
- [Events Documentation](../official_docs/events/events.md)
- [A2A Protocol Roadmap](./a2a.md)

**Note**: This is a roadmap document. The APIs and examples shown here are illustrative and subject to change during implementation.

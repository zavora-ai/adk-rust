# Workflow Patterns

## Selection rules
- Single reasoning chain: `LlmAgent`
- Ordered multi-stage pipeline: `SequentialAgent`
- Independent analyzers: `ParallelAgent`
- Iterative refinement: `LoopAgent`

## Tests to add
- expected sub-agent execution order
- loop termination path
- callback short-circuit behavior
- tool call and response handoff integrity

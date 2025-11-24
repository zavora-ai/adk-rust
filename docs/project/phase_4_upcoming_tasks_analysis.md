# Upcoming Tasks Analysis

**Date**: 2025-11-23  
**Current Status**: Phase 5 Task 5.2 Complete (LlmAgent with function calling)  
**Workspace Tests**: 59 passing

---

## Current Position

### ✅ Completed (Phases 1-5 Partial)
- **Phase 1**: Foundation (core traits, errors, types)
- **Phase 2**: Session & Storage (session, artifact, memory services)
- **Phase 3**: Model Integration (Gemini client, streaming, function calling)
- **Phase 4**: Tool System (FunctionTool, built-ins, toolsets)
- **Phase 5 (Partial)**: 
  - ✅ Task 5.1: CustomAgent
  - ✅ Task 5.2: LlmAgent (basic + function calling)
  - ❌ Task 5.3-5.7: WorkflowAgents, advanced features

---

## Immediate Next Tasks (Phase 5 Completion)

### **Priority 1: Complete Phase 5 - Agent Implementation**

#### **Task 5.3: WorkflowAgents** ⭐ HIGH PRIORITY
**Effort**: 2-3 days  
**Complexity**: Medium  
**Dependencies**: None (can start now)

**What to Build**:
```rust
// Sequential Agent - runs sub-agents in order
pub struct SequentialAgent {
    name: String,
    sub_agents: Vec<Arc<dyn Agent>>,
}

// Parallel Agent - runs sub-agents concurrently
pub struct ParallelAgent {
    name: String,
    sub_agents: Vec<Arc<dyn Agent>>,
}

// Conditional Agent - runs sub-agents based on conditions
pub struct ConditionalAgent {
    name: String,
    condition: Box<dyn Fn(&Context) -> bool>,
    if_agent: Arc<dyn Agent>,
    else_agent: Option<Arc<dyn Agent>>,
}
```

**Why Important**:
- Completes FR-1.6 (workflow agents)
- Enables complex agent orchestration
- Required for real-world applications
- Relatively straightforward to implement

**Tests Needed**:
- Sequential execution order
- Parallel concurrent execution
- Conditional branching
- Event aggregation

---

## Critical Path Analysis

### **Path to Minimal Viable ADK**

```
Current State
    ↓
Task 5.3: WorkflowAgents (2-3 days)
    ↓
Task 6.1: InvocationContext (1-2 days)
    ↓
Task 6.3: Runner Core (3-4 days)
    ↓
Task 8.3: Simple Examples (1-2 days)
    ↓
MILESTONE: Functional ADK
```

**Total Effort**: ~10-14 days to minimal viable system

---

## Task Dependency Graph

```
Phase 5 (Agents)
├─ 5.3 WorkflowAgents ──┐
├─ 5.4 State Mgmt ──────┤
└─ 5.5-5.7 Advanced ────┤
                        ↓
Phase 6 (Runner)        │
├─ 6.1 Context ←────────┘
├─ 6.2 Callbacks
├─ 6.3 Runner Core ←────┐
└─ 6.4 Agent Transfer   │
                        ↓
Phase 7 (Server)        │
├─ 7.1 REST Foundation  │
├─ 7.2-7.4 Endpoints    │
└─ 7.5 A2A Protocol     │
                        ↓
Phase 8 (CLI)           │
├─ 8.1 CLI Foundation ←─┘
├─ 8.2 Console Mode
└─ 8.3 Examples
```

---

## Detailed Task Breakdown

### **Phase 5: Complete Agent Implementation**

#### **5.3: WorkflowAgents** (NEXT)
- **Files**: `adk-agent/src/workflow/{sequential,parallel,conditional}.rs`
- **Lines**: ~300-400 total
- **Tests**: 3 test files, ~15 tests
- **Blockers**: None
- **Value**: High - enables orchestration

#### **5.4: State Management**
- **Files**: `adk-agent/src/llm_agent/state.rs`
- **Lines**: ~150-200
- **Tests**: 1 test file, ~8 tests
- **Blockers**: None
- **Value**: Medium - needed for OutputKey feature

#### **5.5-5.7: Advanced LlmAgent Features**
- Template variables (`{var}`, `{artifact.name}`)
- Callbacks (before/after model/tool)
- Agent transfer
- Output schema validation
- Streaming support
- **Effort**: 5-7 days total
- **Blockers**: Need Runner context for full implementation
- **Value**: High - feature parity with Go

---

### **Phase 6: Runner & Execution** (Critical)

#### **6.1: InvocationContext** ⭐ CRITICAL
- **Files**: `adk-runner/src/context.rs`
- **Lines**: ~200-300
- **Tests**: 1 test file, ~10 tests
- **Blockers**: None
- **Value**: CRITICAL - required for everything else

**What It Provides**:
```rust
pub trait InvocationContext: ReadonlyContext {
    fn agent(&self) -> Arc<dyn Agent>;
    fn artifacts(&self) -> Option<Arc<dyn Artifacts>>;
    fn memory(&self) -> Option<Arc<dyn Memory>>;
    fn session(&self) -> Arc<dyn SessionService>;
    fn run_config(&self) -> &RunConfig;
    fn end_invocation(&self);
    fn ended(&self) -> bool;
}
```

#### **6.2: Callback System**
- **Files**: `adk-runner/src/callbacks.rs`
- **Lines**: ~150-200
- **Tests**: 1 test file, ~8 tests
- **Blockers**: 6.1 (InvocationContext)
- **Value**: High - enables customization

#### **6.3: Runner Core** ⭐ CRITICAL
- **Files**: `adk-runner/src/runner.rs`
- **Lines**: ~400-500
- **Tests**: 1 test file, ~15 tests
- **Blockers**: 6.1, 6.2
- **Value**: CRITICAL - orchestrates everything

**What It Does**:
- Creates InvocationContext
- Calls agent.run()
- Manages event streaming
- Handles errors
- Applies callbacks

#### **6.4: Agent Transfer**
- **Files**: `adk-runner/src/transfer.rs`
- **Lines**: ~200-250
- **Tests**: 1 test file, ~10 tests
- **Blockers**: 6.3 (Runner)
- **Value**: High - enables agent delegation

---

### **Phase 7: Server & API** (Optional for MVP)

Can be deferred until core functionality is solid.

**Priority Order**:
1. 7.1: REST Foundation (needed for deployment)
2. 7.3: Runtime Endpoints (agent execution)
3. 7.2: Session Endpoints (session management)
4. 7.4: Artifact Endpoints (file handling)
5. 7.5: A2A Protocol (agent-to-agent)

---

### **Phase 8: CLI & Examples** (High Value)

#### **8.3: Simple Examples** ⭐ HIGH VALUE
- **Files**: `examples/{quickstart,tool_usage,workflow}.rs`
- **Lines**: ~300-400 total
- **Tests**: Integration tests
- **Blockers**: 6.3 (Runner)
- **Value**: HIGH - demonstrates capabilities

**Examples to Create**:
1. **Quickstart**: Simple Q&A agent
2. **Tool Usage**: Agent with function tools
3. **Workflow**: Sequential/parallel agents
4. **Agent Transfer**: Hierarchical agents

---

## Effort Estimation

### **By Phase**

| Phase | Tasks Remaining | Estimated Days | Priority |
|-------|----------------|----------------|----------|
| Phase 5 | 5 tasks | 8-12 days | HIGH |
| Phase 6 | 4 tasks | 10-14 days | CRITICAL |
| Phase 7 | 5 tasks | 12-16 days | MEDIUM |
| Phase 8 | 3 tasks | 6-8 days | HIGH |
| Phase 9 | 3 tasks | 8-10 days | LOW |
| Phase 10 | 4 tasks | 8-10 days | MEDIUM |

**Total Remaining**: ~52-70 days (10-14 weeks)

### **To Minimal Viable ADK**

| Task | Days | Cumulative |
|------|------|------------|
| 5.3: WorkflowAgents | 2-3 | 2-3 |
| 6.1: InvocationContext | 1-2 | 3-5 |
| 6.3: Runner Core | 3-4 | 6-9 |
| 8.3: Examples | 1-2 | 7-11 |

**MVP Timeline**: 7-11 days

---

## Risk Analysis

### **High Risk Items**

1. **Runner Core (6.3)** - Complex orchestration logic
   - Mitigation: Study Go implementation thoroughly
   - Mitigation: Start with simple version, iterate

2. **Agent Transfer (6.4)** - Tricky context management
   - Mitigation: Implement after Runner is solid
   - Mitigation: Extensive testing

3. **Streaming (5.7)** - Async complexity
   - Mitigation: Use async_stream crate
   - Mitigation: Test with real Gemini API

### **Medium Risk Items**

1. **Callbacks (6.2)** - Type system complexity
   - Mitigation: Use trait objects with Arc
   - Mitigation: Keep callback signatures simple

2. **State Management (5.4)** - Mutation tracking
   - Mitigation: Use EventActions.StateDelta pattern
   - Mitigation: Test state isolation

### **Low Risk Items**

1. **WorkflowAgents (5.3)** - Straightforward logic
2. **Examples (8.3)** - Documentation-focused
3. **REST API (7.1-7.4)** - Well-understood patterns

---

## Recommended Approach

### **Sprint 1: Complete Phase 5** (8-12 days)

**Week 1**:
- Day 1-3: Task 5.3 - WorkflowAgents
- Day 4-5: Task 5.4 - State Management
- Day 6-7: Start Task 5.5 - Template Variables

**Week 2**:
- Day 8-10: Complete Task 5.5 - Template Variables
- Day 11-12: Task 5.6 - Callbacks (partial)

**Deliverable**: All agent types working

### **Sprint 2: Phase 6 Foundation** (10-14 days)

**Week 3**:
- Day 1-2: Task 6.1 - InvocationContext
- Day 3-5: Task 6.2 - Callback System
- Day 6-7: Start Task 6.3 - Runner Core

**Week 4**:
- Day 8-12: Complete Task 6.3 - Runner Core
- Day 13-14: Task 6.4 - Agent Transfer

**Deliverable**: End-to-end execution working

### **Sprint 3: Examples & Polish** (6-8 days)

**Week 5**:
- Day 1-2: Task 8.3 - Simple Examples
- Day 3-4: Integration testing
- Day 5-6: Documentation
- Day 7-8: Bug fixes & polish

**Deliverable**: Functional ADK with examples

---

## Success Metrics

### **Phase 5 Complete**
- ✅ All agent types implemented
- ✅ 70+ workspace tests passing
- ✅ Template variables working
- ✅ Callbacks functional

### **Phase 6 Complete**
- ✅ Runner orchestrates agents
- ✅ Agent transfer working
- ✅ 85+ workspace tests passing
- ✅ End-to-end integration tests

### **MVP Complete**
- ✅ 4+ working examples
- ✅ 90+ workspace tests passing
- ✅ Documentation complete
- ✅ Can build real applications

---

## Decision Points

### **Should We Implement Streaming Now?**
**Recommendation**: Defer to Sprint 2
- Not critical for MVP
- Adds complexity
- Can add after Runner is stable

### **Should We Build REST API Before Examples?**
**Recommendation**: Examples first
- Examples validate core functionality
- REST API can be built on solid foundation
- Examples serve as integration tests

### **Should We Implement All WorkflowAgents?**
**Recommendation**: Start with Sequential & Parallel
- Conditional can come later
- Loop agent is less common
- Focus on high-value features

---

## Next Immediate Actions

### **This Week**

1. **Start Task 5.3: WorkflowAgents**
   - Implement SequentialAgent
   - Implement ParallelAgent
   - Write comprehensive tests
   - Target: 2-3 days

2. **Plan Task 6.1: InvocationContext**
   - Study Go implementation
   - Design Rust trait hierarchy
   - Plan service integration
   - Target: Start next week

3. **Update Documentation**
   - Document WorkflowAgent patterns
   - Add architecture diagrams
   - Update progress tracking

---

## Conclusion

**Current Position**: 70% complete (5 of 10 phases)

**Path to MVP**: 
1. Complete WorkflowAgents (2-3 days)
2. Build InvocationContext (1-2 days)
3. Implement Runner Core (3-4 days)
4. Create Examples (1-2 days)

**Total to MVP**: 7-11 days

**Key Insight**: We're closer than it seems. The foundation is solid, and the remaining work is mostly orchestration and integration. Focus on the critical path (Runner) and defer nice-to-have features (REST API, advanced streaming) until core functionality is proven.

**Recommendation**: Start with Task 5.3 (WorkflowAgents) immediately. It's low-risk, high-value, and has no blockers.

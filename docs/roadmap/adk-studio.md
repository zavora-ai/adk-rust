# ADK Studio

*Priority: 🔴 P0 | Target: Q1-Q2 2025 | Effort: 8 weeks*

## Overview

Build a visual, low-code development environment for ADK-Rust agents, matching AutoGen Studio capabilities.

## Problem Statement

Currently, building agents with ADK-Rust requires:
- Writing Rust code for every agent
- Manual workflow orchestration
- CLI-based testing
- No visual debugging

AutoGen Studio provides:
- Drag-and-drop agent builder
- Visual workflow editor
- Live testing sandbox
- No-code prototyping

## Proposed Solution

### ADK Studio Web Application

A React-based web application for visual agent development:

```
┌─────────────────────────────────────────────────────────────┐
│  ADK Studio                                    [Save] [Run] │
├─────────────────────────────────────────────────────────────┤
│ ┌─────────┐ ┌───────────────────────────────────────────┐   │
│ │ Agents  │ │                                           │   │
│ │ ───────┐│ │    ┌─────────┐      ┌─────────┐          │   │
│ │ 📦 LLM ││ │    │Research │─────▶│ Writer  │          │   │
│ │ 📦 Tool││ │    │  Agent  │      │  Agent  │          │   │
│ │ 📦 Work││ │    └─────────┘      └────┬────┘          │   │
│ ├─────────┤ │                          │               │   │
│ │ Tools   │ │                          ▼               │   │
│ │ ───────┐│ │                    ┌─────────┐          │   │
│ │ 🔧 Goog││ │                    │ Reviewer│          │   │
│ │ 🔧 Web ││ │                    │  Agent  │          │   │
│ │ 🔧 Code││ │                    └─────────┘          │   │
│ └─────────┘ └───────────────────────────────────────────┘   │
│ ┌───────────────────────────────────────────────────────┐   │
│ │ Properties: Research Agent                             │   │
│ │ Model: gemini-2.5-flash  Instruction: [............]  │   │
│ │ Tools: [Google Search] [Web Browse]                    │   │
│ └───────────────────────────────────────────────────────┘   │
└─────────────────────────────────────────────────────────────┘
```

## Core Features

### 1. Visual Agent Builder

| Feature | Description |
|---------|-------------|
| Agent Palette | Drag LLM, Tool, Workflow agents onto canvas |
| Property Editor | Configure model, instructions, tools |
| Connection Editor | Draw edges between agents |
| Validation | Real-time validation of agent configs |

### 2. Workflow Editor

| Feature | Description |
|---------|-------------|
| Graph Canvas | Visual node-edge editor |
| Node Types | Agent, Condition, Loop, Parallel, Start, End |
| Edge Types | Sequential, Conditional, Parallel |
| State Inspector | View state at each node |

### 3. Live Testing

| Feature | Description |
|---------|-------------|
| Chat Interface | Test agents in real-time |
| Event Stream | View all events as they happen |
| State Timeline | Scrub through execution history |
| Breakpoints | Pause at specific nodes |

### 4. Code Export

```rust
// Generated from ADK Studio workflow
let research_agent = LlmAgentBuilder::new("research")
    .model(gemini_model.clone())
    .instruction("Research the topic thoroughly")
    .tools(vec![google_search, web_browse])
    .build()?;

let writer_agent = LlmAgentBuilder::new("writer")
    .model(gemini_model.clone())
    .instruction("Write a comprehensive article")
    .build()?;

let workflow = GraphAgent::builder("content_pipeline")
    .node(AgentNode::new(research_agent))
    .node(AgentNode::new(writer_agent))
    .edge(START, "research")
    .edge("research", "writer")
    .edge("writer", END)
    .build()?;
```

## Architecture

```
┌────────────────────────────────────────────────────────────┐
│                     ADK Studio Frontend                     │
│  ┌──────────────┐ ┌──────────────┐ ┌──────────────┐       │
│  │ Agent Builder│ │Workflow Edit │ │ Test Console │       │
│  └──────┬───────┘ └──────┬───────┘ └──────┬───────┘       │
│         │                │                │                │
│         └────────────────┼────────────────┘                │
│                          │                                 │
│                    React + TypeScript                       │
└──────────────────────────┼─────────────────────────────────┘
                           │ WebSocket / REST
┌──────────────────────────┼─────────────────────────────────┐
│                          │                                 │
│  ┌───────────────────────▼────────────────────────────┐   │
│  │                 ADK Studio Server                   │   │
│  │  ┌──────────┐ ┌──────────┐ ┌──────────┐           │   │
│  │  │ Project  │ │  Agent   │ │  Runner  │           │   │
│  │  │ Manager  │ │ Compiler │ │  Engine  │           │   │
│  │  └──────────┘ └──────────┘ └──────────┘           │   │
│  └────────────────────────────────────────────────────┘   │
│                                                            │
│                     Rust (adk-studio)                      │
└────────────────────────────────────────────────────────────┘
```

## Implementation Plan

### Weeks 1-2: Backend Foundation
- [ ] `adk-studio` crate structure
- [ ] Project/workflow JSON schema
- [ ] Agent compilation from JSON
- [ ] REST API endpoints
- [ ] WebSocket for live updates

### Weeks 3-4: Frontend Canvas
- [ ] React app with React Flow
- [ ] Agent palette component
- [ ] Drag-and-drop to canvas
- [ ] Node property editor
- [ ] Edge connections

### Weeks 5-6: Workflow Editor
- [ ] Graph node types
- [ ] Condition editor (code/visual)
- [ ] Loop configuration
- [ ] Parallel execution groups
- [ ] State channel definition

### Weeks 7-8: Testing & Export
- [ ] Chat testing interface
- [ ] Event stream viewer
- [ ] Execution timeline
- [ ] Rust code export
- [ ] Template gallery

## Tech Stack

| Component | Technology |
|-----------|------------|
| Frontend | React, TypeScript, React Flow, Tailwind |
| Backend | Rust, Axum, adk-server |
| State | Zustand or Redux |
| Canvas | React Flow or Rete.js |
| Code Gen | Handlebars templates |

## Success Metrics

- [ ] Create agent in <2 minutes without code
- [ ] Export generates compilable Rust code
- [ ] <500ms latency for live testing
- [ ] Import/export project files

## Related

- [AutoGen Studio](https://microsoft.github.io/autogen/docs/autogen-studio/getting-started)
- [LangGraph Studio](https://langchain-ai.github.io/langgraph/concepts/langgraph_studio/)
- [adk-graph](../../adk-graph/) - Graph agent foundation

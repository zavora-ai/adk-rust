# ADK Studio

*Priority: 🔴 P0 | Target: Q1-Q2 2026 | Effort: 8 weeks*

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
│  🔧 ADK Studio    File ▾   Templates ▾   Help ▾    [Build] │
├─────────────────────────────────────────────────────────────┤
│ ┌─────────┐ ┌───────────────────────────────────────────┐   │
│ │ Agents  │ │                                           │   │
│ │ ───────┐│ │    ┌─────────┐      ┌─────────┐          │   │
│ │ 📦 LLM ││ │    │Research │─────▶│ Writer  │          │   │
│ │ 📦 Seq ││ │    │  Agent  │      │  Agent  │          │   │
│ │ 📦 Loop││ │    └─────────┘      └────┬────┘          │   │
│ │ 📦 Par ││ │                          │               │   │
│ │ 📦 Rout││ │                          ▼               │   │
│ ├─────────┤ │                    ┌─────────┐          │   │
│ │ Tools   │ │                    │ Reviewer│          │   │
│ │ ───────┐│ │                    │  Agent  │          │   │
│ │ 🔧 Func││ │                    └─────────┘          │   │
│ │ 🔧 MCP ││ │                                          │   │
│ │ 🔧 Brow││ └───────────────────────────────────────────┘   │
│ │ 🔧 Srch││ ┌───────────────────────────────────────────┐   │
│ └─────────┘ │ 💬 Test Console                    [Trace]│   │
│ ┌─────────┐ │ > Hello                                   │   │
│ │Properties│ │ 🤖 Hi! How can I help?                   │   │
│ │ Model   │ └───────────────────────────────────────────┘   │
│ │ Instruct│                                                 │
│ └─────────┘                                                 │
└─────────────────────────────────────────────────────────────┘
```

## Core Features

### 1. Visual Agent Builder

| Feature | Status | Description |
|---------|--------|-------------|
| Agent Palette | ✅ Done | Drag LLM, Sequential, Loop, Parallel, Router agents |
| Property Editor | ✅ Done | Configure model, instructions, tools |
| Connection Editor | ✅ Done | Draw edges between agents |
| Validation | ✅ Done | Real-time validation of agent configs |
| Sub-agent Management | ✅ Done | Add/remove sub-agents in containers |

### 2. Workflow Editor

| Feature | Status | Description |
|---------|--------|-------------|
| Graph Canvas | ✅ Done | Visual node-edge editor with React Flow |
| Node Types | ✅ Done | LLM, Sequential, Loop, Parallel, Router, Start, End |
| Edge Types | ✅ Done | Sequential, Conditional (router) |
| State Inspector | 🔲 Pending | View state at each node |
| Auto-Layout | 🔲 Pending | Automatic graph layout algorithms |

### 3. Live Testing

| Feature | Status | Description |
|---------|--------|-------------|
| Chat Interface | ✅ Done | Test agents in real-time |
| Event Stream | ✅ Done | View all events as they happen |
| Active Agent Highlight | ✅ Done | Visual indicator of running agent |
| Iteration Counter | ✅ Done | Show loop iteration progress |
| State Timeline | 🔲 Pending | Scrub through execution history |
| Breakpoints | 🔲 Pending | Pause at specific nodes |

### 4. Code Export

| Feature | Status | Description |
|---------|--------|-------------|
| Rust Code Generation | ✅ Done | Complete main.rs with all agents |
| Cargo.toml Generation | ✅ Done | Correct dependencies |
| Build & Run | ✅ Done | Compile and execute from UI |
| Code Editor View | ✅ Done | Monaco editor with syntax highlighting |

### 5. Templates & Menu

| Feature | Status | Description |
|---------|--------|-------------|
| MenuBar | ✅ Done | File, Templates, Help menus |
| Template Gallery | ✅ Done | 7 ready-to-run templates |
| New Project | ✅ Done | Create from menu |
| Export Code | ✅ Done | View generated code |

## UI/UX Requirements

### Layout & Canvas

| Requirement | Status | Description |
|-------------|--------|-------------|
| Auto-Layout | 🔲 Pending | Dagre/ELK layout for automatic node positioning |
| Fit to View | 🔲 Pending | Button to zoom/pan to show all nodes |
| Mini-Map | 🔲 Pending | Overview for large graphs |
| Zoom Controls | ✅ Done | React Flow built-in controls |
| Pan & Zoom | ✅ Done | Mouse/trackpad navigation |
| Grid Snap | 🔲 Pending | Snap nodes to grid |
| Node Alignment | 🔲 Pending | Align selected nodes |

### Interaction

| Requirement | Status | Description |
|-------------|--------|-------------|
| Drag & Drop Agents | ✅ Done | From palette to canvas |
| Drag & Drop Tools | ✅ Done | From palette onto agents |
| Click to Select | ✅ Done | Select agent to edit properties |
| Multi-Select | 🔲 Pending | Shift+click or box select |
| Copy/Paste | 🔲 Pending | Duplicate agents |
| Undo/Redo | 🔲 Pending | History stack |
| Keyboard Shortcuts | 🔲 Pending | Delete, copy, paste, etc. |
| Context Menu | 🔲 Pending | Right-click options |

### Visual Feedback

| Requirement | Status | Description |
|-------------|--------|-------------|
| Active Agent Glow | ✅ Done | Green highlight during execution |
| Selected Agent Ring | ✅ Done | Blue ring on selected |
| Edge Animation | 🔲 Pending | Animated flow during execution |
| Error Indicators | 🔲 Pending | Red highlight on invalid config |
| Loading States | ✅ Done | Build progress indicator |
| Tool Badges | ✅ Done | Show tools on agent nodes |

### Responsive Design

| Requirement | Status | Description |
|-------------|--------|-------------|
| Resizable Panels | 🔲 Pending | Drag to resize palette/properties/console |
| Collapsible Panels | 🔲 Pending | Hide/show panels |
| Mobile Support | 🔲 Pending | Touch-friendly on tablets |
| Dark Theme | ✅ Done | Default dark theme |
| Light Theme | 🔲 Pending | Optional light theme |

### Accessibility

| Requirement | Status | Description |
|-------------|--------|-------------|
| Keyboard Navigation | 🔲 Pending | Tab through elements |
| Screen Reader | 🔲 Pending | ARIA labels |
| High Contrast | 🔲 Pending | Accessible color scheme |
| Focus Indicators | 🔲 Pending | Visible focus states |

## Tool Support

| Tool | Status | Description |
|------|--------|-------------|
| Function Tool | ✅ Done | Custom code with parameters |
| MCP Tool | ✅ Done | Model Context Protocol servers |
| Browser Tool | ✅ Done | Web browsing capabilities |
| Google Search | ✅ Done | Grounding with search |
| Exit Loop | ✅ Done | Break out of loop agents |
| Load Artifact | ✅ Done | Load saved artifacts |

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
│                    React Flow + Zustand                     │
└──────────────────────────┼─────────────────────────────────┘
                           │ SSE / REST
┌──────────────────────────┼─────────────────────────────────┐
│                          │                                 │
│  ┌───────────────────────▼────────────────────────────┐   │
│  │                 ADK Studio Server                   │   │
│  │  ┌──────────┐ ┌──────────┐ ┌──────────┐           │   │
│  │  │ Project  │ │  Code    │ │  Build   │           │   │
│  │  │ Storage  │ │ Generator│ │  Runner  │           │   │
│  │  └──────────┘ └──────────┘ └──────────┘           │   │
│  └────────────────────────────────────────────────────┘   │
│                                                            │
│                     Rust (adk-studio)                      │
└────────────────────────────────────────────────────────────┘
```

## Implementation Progress

### ✅ Phase 1: Backend Foundation (Complete)
- [x] `adk-studio` crate structure
- [x] Project/workflow JSON schema
- [x] Agent compilation from JSON (codegen)
- [x] REST API endpoints
- [x] SSE for live streaming

### ✅ Phase 2: Frontend Canvas (Complete)
- [x] React app with React Flow
- [x] Agent palette component
- [x] Drag-and-drop to canvas
- [x] Node property editor
- [x] Edge connections
- [x] Tool palette with drag onto agents

### ✅ Phase 3: Agent Types (Complete)
- [x] LLM Agent
- [x] Sequential Agent (with sub-agents)
- [x] Loop Agent (with max_iterations, exit_loop)
- [x] Parallel Agent
- [x] Router Agent (with routes)

### ✅ Phase 4: Testing & Export (Complete)
- [x] Chat testing interface
- [x] Event stream viewer (trace tab)
- [x] Rust code export
- [x] Build from UI
- [x] Template gallery (7 templates)
- [x] MenuBar (File, Templates, Help)

### 🔲 Phase 5: UI Polish (Pending)
- [ ] Auto-layout (Dagre/ELK)
- [ ] Fit to view
- [ ] Mini-map
- [ ] Resizable panels
- [ ] Undo/Redo
- [ ] Copy/Paste
- [ ] Keyboard shortcuts
- [ ] Edge animation during execution

### 🔲 Phase 6: Debugging (Pending)
- [ ] State inspector
- [ ] Execution timeline with scrubbing
- [ ] Breakpoints
- [ ] Step-through execution

### 🔲 Phase 7: Advanced (Future)
- [ ] Project import
- [ ] Version history
- [ ] Collaboration features
- [ ] Deploy to cloud

## Tech Stack

| Component | Technology |
|-----------|------------|
| Frontend | React 18, TypeScript, React Flow, Tailwind |
| Backend | Rust, Axum, adk-studio |
| State | Zustand |
| Canvas | React Flow |
| Code Editor | Monaco Editor |
| Code Gen | Rust string templates |

## Success Metrics

| Metric | Status |
|--------|--------|
| Create agent in <2 minutes without code | ✅ Achieved |
| Export generates compilable Rust code | ✅ Achieved |
| <500ms latency for live testing | ✅ Achieved |
| Import/export project files | 🔲 Export only |

## Test Coverage

- 26 integration tests covering all agent types
- Codegen demo example for all templates

## Templates Included

1. 💬 Simple Chat Agent - Basic conversational agent
2. 🔍 Research Pipeline - Sequential: Researcher → Summarizer
3. ✨ Content Refiner - Loop agent with iterative improvement
4. ⚡ Parallel Analyzer - Concurrent sentiment + entity extraction
5. 🔀 Support Router - Route to tech/billing/general agents
6. 🌐 Web Browser Agent - LLM with browser tools

## Related

- [AutoGen Studio](https://microsoft.github.io/autogen/docs/autogen-studio/getting-started)
- [LangGraph Studio](https://langchain-ai.github.io/langgraph/concepts/langgraph_studio/)
- [adk-graph](../../adk-graph/) - Graph agent foundation

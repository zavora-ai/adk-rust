# ADK Studio

*Priority: 🔴 P0 | Target: Q1-Q2 2026 | Effort: 8 weeks*

> **📋 Status**: ~90% complete | **Last Updated**: 2026-01-24

## Overview

Build a visual, low-code development environment for ADK-Rust agents, matching AutoGen Studio capabilities.

## Flowgram-Informed Improvements (Spec, Design, Task Plan)

### Why Flowgram Matters (Observed)

Based on the Flowgram README in `/tmp/flowgram.ai`, the appeal comes from:
- Clean, light UI with generous spacing and quick visual parsing.
- Comprehensive workflow tooling: free/fixed layout canvas, node forms, variable scope, material library.
- Demo-first onboarding: runnable templates and clear quick start. 

ADK Studio should adopt the clarity and completeness while staying Rust-first and agent-centric.

### Current ADK Studio Snapshot (As Implemented)

**UI + UX**
- Dark theme defaults (Tailwind tokens in `adk-studio/ui/tailwind.config.js`).
- ReactFlow canvas with grid background, controls, MiniMap, animated edges.
- Left palette (Agents + Tools), right properties/tool config panels.
- Bottom console for build/run + SSE event stream.

**Key Capabilities (from current code + README)**
- Agent palette: LLM, Sequential, Parallel, Loop, Router.
- Tool palette: Function, MCP, Browser, Google Search, Load Artifact, Exit Loop.
- Codegen + build + run pipeline with SSE build output.
- Templates via Menu Bar with auto-layout.
- Execution visualization hooks: active node glow, iteration counter, thought bubble wiring.

**Gaps vs Flowgram-Style Expectations**
- No light theme; dark-only reduces readability for dense graphs.
- No free-layout mode; layout is graph-first with auto-layout.
- Forms are functional but not schema-driven; validation is minimal.
- No variable/state inspector or data-flow overlays.
- No timeline/snapshot debugging view.

### Product Spec (Studio vNext)

**Goals**
- Make ADK Studio the fastest path from “idea” to “running agent system” in Rust.
- Improve “readability at a glance” for complex multi-agent graphs (light theme + data flow clarity).
- Preserve ADK identity: agent + tool composition, production build output, and local-first workflows.

**Non-Goals**
- Not a generic workflow framework; studio remains ADK-specific.
- Not a cloud-hosted SaaS (local-first remains the default).

**Primary Users**
- Rust engineers prototyping agent systems
- Teams evaluating ADK for production
- Educators and demo builders

**Key User Journeys**
1. Open Studio → pick “Agent System Template” → run → edit nodes.
2. Build a graph, inspect data flow, export Rust code.
3. Debug an execution with trace + state snapshots.

**Feature Pillars**
1. **Visual Workflow Clarity**
   - Free-layout canvas for creative exploration.
   - Fixed-layout / structured canvas for production flows.
   - Lightweight “data flow” overlays: show which state keys move across edges.
2. **Configuration Experience**
   - Schema-driven node forms (tools, models, memory, auth).
   - Inline validation and “fix suggestions” for invalid configs.
3. **Runtime Feedback**
   - Timeline view with state snapshots at each node.
   - Console + event stream with filtered views (model/tool/session).
4. **Template-First Onboarding**
   - 8–12 curated templates (agent teams, eval loop, tool-heavy, realtime).
   - One-click run, editable in canvas.
5. **Production Path**
   - “Export” always generates minimal, readable Rust.
   - Build/Run stays local; surface warnings when missing env vars.

**Acceptance Criteria**
- New user can create, run, and export a working agent system in < 10 minutes.
- Visual graph can be understood at a glance in light theme (no heavy contrast).
- Studio exports compile without manual edits for templates.

**Scope Notes (Based on Current Codebase)**
- Light theme needs new Tailwind tokens + CSS variables; ReactFlow grid and MiniMap colors must adapt.
- Free-layout can be a new layout mode alongside Dagre layout (`useLayout.ts`, `layout/modes.ts`).
- Forms can be upgraded incrementally: start with schema-driven validation for tools and output schemas.
- State inspector and data overlays can build on SSE events and execution state in `useExecution`.

### Design Direction (Light Theme, ADK Identity)

**Design Principles**
- **Engineering-first**: clean, precise, minimal decoration.
- **Readable at scale**: 20+ nodes should remain legible.
- **ADK distinct**: structured, technical aesthetic (not “playful SaaS”).

**Visual Language**
- Light theme default, dark theme optional.
- Subtle depth: soft shadows, light borders, layered panels.
- Accent color unique to ADK (suggested: deep teal + amber for state warnings).

**Core UI Elements**
- **Canvas**: off-white background with faint grid; nodes with clear headers.
- **Nodes**: tighter type scale; icons for agent types; tool badges as chips.
- **Edges**: clean lines; optional data overlays for state keys.
- **Side Panels**: forms on right; palette on left; console bottom.
- **Theme Tokens (draft)**
  - Background: `#F7F8FA`
  - Surface: `#FFFFFF`
  - Border: `#E3E6EA`
  - Text: `#1C232B`
  - Accent: `#0F8A8A` (primary), `#F59F00` (warning)

**Differentiators vs Flowgram**
- Emphasize agent thinking loops and tool calls.
- Native Rust code export and run pipeline.
- Tight integration with ADK runtime, sessions, telemetry.

**Design Upgrade Targets (Grounded in Current UI)**
- Replace dark defaults in `index.css` and Tailwind tokens with theme variables.
- Improve node readability: stronger headers, subtle cards, clearer tool chips.
- Make console collapsible with summary status line (build/run/last error).
- Add a “data lane” on edges for state key labels (toggle).

### Task Plan (6–8 Weeks)

**Phase 1 — Discovery & Design (Week 1–2)**
- Audit current Studio UI for friction points (canvas, forms, console).
- Map Flowgram features to ADK equivalents: free/fixed layouts, variable scope, form engine.
- Produce light theme style guide + component tokens.
- Define “Data Flow Overlay” spec.

**Phase 2 — Foundation UI (Week 3–4)**
- Implement light theme tokens + theme switch.
- Refine canvas readability: grid, spacing, node density.
- Upgrade node form UX (grouped sections, validation messages).

**Phase 3 — Workflow Clarity (Week 5–6)**
- Add data-flow overlays (state keys on edges).
- Add variable/state inspector panel.
- Add timeline view for execution snapshots.

**Phase 4 — Templates & Onboarding (Week 7–8)**
- Curate 8–12 templates with “Run” CTA.
- Build “first-run” walkthrough.
- Add code export polish (commented sections, minimal dependencies).

**Validation**
- Record 3 user sessions using light theme.
- Measure: time-to-first-run, number of validation errors, export success rate.

### Deliverables
- Light theme in Studio (optional dark mode).
- Data flow overlays + state inspector.
- Timeline debugging view.
- Template gallery v2.
- Updated Studio onboarding and export flow.

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
| Auto-Layout | ✅ Done | Dagre layout with LR/TB toggle via useLayout.ts |

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
| Template Gallery | ✅ Done | 6 ready-to-run templates |
| New Project | ✅ Done | Create from menu |
| Export Code | ✅ Done | View generated code |

## UI/UX Requirements

### Layout & Canvas

| Requirement | Status | Description |
|-------------|--------|-------------|
| Auto-Layout | ✅ Done | Dagre layout with LR/TB toggle via CanvasToolbar |
| Fit to View | ✅ Done | Button in toolbar + Ctrl+0 shortcut |
| Mini-Map | ✅ Done | React Flow MiniMap with active node coloring |
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
| Keyboard Shortcuts | ✅ Done | Delete, Ctrl+D duplicate, Ctrl+L layout, Ctrl+0 fit, Esc |
| Context Menu | 🔲 Pending | Right-click options |

### Visual Feedback

| Requirement | Status | Description |
|-------------|--------|-------------|
| Active Agent Glow | ✅ Done | Green highlight during execution |
| Selected Agent Ring | ✅ Done | Blue ring on selected |
| Edge Animation | ✅ Done | AnimatedEdge with CSS dash animation |
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
- [x] Template gallery (6 templates)
- [x] MenuBar (File, Templates, Help)

### ✅ Phase 5: UI Polish (Mostly Complete)
- [x] Auto-layout (Dagre with LR/TB toggle)
- [x] Fit to view (button + Ctrl+0)
- [x] Mini-map (with active node coloring)
- [x] Keyboard shortcuts (Delete, Ctrl+D, Ctrl+L, Ctrl+0, Esc)
- [x] Edge animation during execution (AnimatedEdge.tsx)
- [x] Thought bubbles (ThoughtBubble.tsx with Framer Motion)
- [ ] Resizable panels
- [ ] Undo/Redo
- [ ] Copy/Paste (Ctrl+D duplicate exists)
- [ ] Multi-select
- [ ] Context menu

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

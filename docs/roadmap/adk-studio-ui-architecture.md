# ADK Studio UI Architecture Plan

*Priority: 🔴 P0 | Effort: 2-3 weeks*

## Overview

Refactor the monolithic Canvas.tsx (~1500 lines) into a modular, maintainable architecture that supports advanced agentic visualizations including thought bubbles, semantic zoom, and adaptive layouts.

## Current State (Problem)

```
Canvas.tsx (1500+ lines)
├── Node rendering
├── Edge rendering  
├── Drag & drop
├── Properties panel
├── Tool config
├── Build logic
├── SSE handling
├── State management
└── Everything else...
```

**Issues:**
- Single file responsibility overload
- Hard to test individual features
- Difficult to add new node types
- No separation of concerns
- Tight coupling between UI and logic

---

## Target Architecture

```
src/
├── components/
│   ├── Canvas/
│   │   ├── index.tsx              # Main canvas container (thin)
│   │   ├── CanvasToolbar.tsx      # Zoom, layout, fit buttons
│   │   └── CanvasControls.tsx     # React Flow controls wrapper
│   │
│   ├── Nodes/
│   │   ├── index.ts               # Export all node types
│   │   ├── BaseNode.tsx           # Shared node wrapper
│   │   ├── LlmAgentNode.tsx       # LLM agent visualization
│   │   ├── SequentialNode.tsx     # Sequential container
│   │   ├── LoopNode.tsx           # Loop with iteration indicator
│   │   ├── ParallelNode.tsx       # Parallel container
│   │   ├── RouterNode.tsx         # Router with branches
│   │   └── ThoughtBubble.tsx      # LLM reasoning overlay
│   │
│   ├── Edges/
│   │   ├── index.ts
│   │   ├── BaseEdge.tsx           # Standard edge
│   │   ├── AnimatedEdge.tsx       # Flow animation during execution
│   │   └── ConditionalEdge.tsx    # Router condition labels
│   │
│   ├── Panels/
│   │   ├── AgentPalette.tsx       # Draggable agent types
│   │   ├── ToolPalette.tsx        # Draggable tools
│   │   ├── PropertiesPanel.tsx    # Agent/tool config
│   │   └── ExecutionPanel.tsx     # Live execution state
│   │
│   ├── Console/
│   │   ├── TestConsole.tsx        # Chat interface
│   │   ├── TraceViewer.tsx        # Event stream
│   │   └── StateInspector.tsx     # Runtime state view
│   │
│   ├── MenuBar/
│   │   ├── MenuBar.tsx            # (existing)
│   │   └── templates.ts           # (existing)
│   │
│   └── Overlays/
│       ├── ThoughtBubble.tsx      # Floating reasoning display
│       ├── ToolCallPopup.tsx      # Tool execution details
│       └── MiniMap.tsx            # Graph overview
│
├── hooks/
│   ├── useSSE.ts                  # (existing) SSE streaming
│   ├── useLayout.ts               # Auto-layout logic
│   ├── useExecution.ts            # Execution state tracking
│   ├── useNodeActions.ts          # Node CRUD operations
│   ├── useEdgeActions.ts          # Edge CRUD operations
│   └── useKeyboardShortcuts.ts    # Hotkeys
│
├── layout/
│   ├── index.ts
│   ├── analyzer.ts                # Graph pattern detection
│   ├── dagre.ts                   # Dagre layout
│   ├── elk.ts                     # ELK layout (future)
│   └── modes.ts                   # Layout mode definitions
│
├── store/
│   ├── index.ts                   # (existing) Main store
│   ├── slices/
│   │   ├── projectSlice.ts        # Project state
│   │   ├── canvasSlice.ts         # Canvas UI state
│   │   ├── executionSlice.ts      # Runtime state
│   │   └── uiSlice.ts             # Panels, selection, etc.
│   └── selectors.ts               # Derived state
│
├── types/
│   ├── project.ts                 # (existing)
│   ├── nodes.ts                   # Node type definitions
│   ├── execution.ts               # Runtime types
│   └── layout.ts                  # Layout types
│
└── utils/
    ├── nodeFactory.ts             # Create nodes from agents
    ├── edgeFactory.ts             # Create edges
    └── graphUtils.ts              # Graph helpers
```

---

## Component Specifications

### 1. Node Components

#### BaseNode.tsx
Shared wrapper for all node types.

```typescript
interface BaseNodeProps {
  id: string;
  label: string;
  icon: string;
  isActive: boolean;
  isSelected: boolean;
  children: React.ReactNode;
  onSelect: () => void;
}

export function BaseNode({ 
  id, label, icon, isActive, isSelected, children, onSelect 
}: BaseNodeProps) {
  return (
    <div 
      className={cn(
        'node-base',
        isActive && 'node-active',
        isSelected && 'node-selected'
      )}
      onClick={onSelect}
    >
      <Handle type="target" position={Position.Top} />
      <div className="node-header">
        <span className="node-icon">{icon}</span>
        <span className="node-label">{label}</span>
      </div>
      <div className="node-body">{children}</div>
      <Handle type="source" position={Position.Bottom} />
    </div>
  );
}
```

#### LlmAgentNode.tsx

```typescript
interface LlmNodeData {
  label: string;
  model: string;
  instruction: string;
  tools: string[];
  isActive: boolean;
  thought?: string;
}

export const LlmAgentNode = memo(({ data, selected }: NodeProps<LlmNodeData>) => {
  return (
    <BaseNode
      id={data.label}
      label={data.label}
      icon="🤖"
      isActive={data.isActive}
      isSelected={selected}
    >
      <div className="text-xs text-gray-400">{data.model}</div>
      <div className="flex flex-wrap gap-1 mt-1">
        {data.tools.map(t => (
          <span key={t} className="tool-badge">{t}</span>
        ))}
      </div>
      
      {data.thought && (
        <ThoughtBubble text={data.thought} streaming={data.isActive} />
      )}
    </BaseNode>
  );
});
```

#### SequentialNode.tsx

```typescript
interface SequentialNodeData {
  label: string;
  subAgents: string[];
  activeSubAgent?: string;
  isActive: boolean;
}

export const SequentialNode = memo(({ data, selected }: NodeProps<SequentialNodeData>) => {
  return (
    <BaseNode label={data.label} icon="⛓" isActive={data.isActive} isSelected={selected}>
      <div className="sub-agents">
        {data.subAgents.map((sub, idx) => (
          <div 
            key={sub}
            className={cn(
              'sub-agent',
              data.activeSubAgent === sub && 'sub-agent-active'
            )}
          >
            <span className="sub-agent-index">{idx + 1}.</span>
            <span className="sub-agent-name">{sub}</span>
          </div>
        ))}
      </div>
    </BaseNode>
  );
});
```

#### LoopNode.tsx

```typescript
interface LoopNodeData {
  label: string;
  subAgents: string[];
  maxIterations: number;
  currentIteration: number;
  activeSubAgent?: string;
  isActive: boolean;
}

export const LoopNode = memo(({ data, selected }: NodeProps<LoopNodeData>) => {
  return (
    <BaseNode label={data.label} icon="🔄" isActive={data.isActive} isSelected={selected}>
      <div className="loop-info">
        <span className="iteration-counter">
          {data.isActive 
            ? `Iteration ${data.currentIteration}/${data.maxIterations}`
            : `Max ${data.maxIterations} iterations`
          }
        </span>
      </div>
      <div className="sub-agents">
        {data.subAgents.map((sub, idx) => (
          <div 
            key={sub}
            className={cn(
              'sub-agent',
              data.activeSubAgent === sub && 'sub-agent-active'
            )}
          >
            {data.activeSubAgent === sub ? '⚡' : `${idx + 1}.`} {sub}
          </div>
        ))}
      </div>
    </BaseNode>
  );
});
```

#### RouterNode.tsx

```typescript
interface RouterNodeData {
  label: string;
  model: string;
  routes: Array<{ condition: string; target: string }>;
  activeRoute?: string;
  isActive: boolean;
}

export const RouterNode = memo(({ data, selected }: NodeProps<RouterNodeData>) => {
  return (
    <BaseNode label={data.label} icon="🔀" isActive={data.isActive} isSelected={selected}>
      <div className="routes">
        {data.routes.map(route => (
          <div 
            key={route.condition}
            className={cn(
              'route',
              data.activeRoute === route.condition && 'route-active'
            )}
          >
            <span className="route-condition">{route.condition}</span>
            <span className="route-arrow">→</span>
            <span className="route-target">{route.target}</span>
          </div>
        ))}
      </div>
    </BaseNode>
  );
});
```

---

### 2. ThoughtBubble Component

The signature feature for agentic visualization.

```typescript
// src/components/Overlays/ThoughtBubble.tsx
import { motion, AnimatePresence } from 'framer-motion';

interface ThoughtBubbleProps {
  text: string;
  position?: 'top' | 'right' | 'bottom';
  streaming?: boolean;
  type?: 'thinking' | 'tool' | 'decision';
}

export function ThoughtBubble({ 
  text, 
  position = 'right', 
  streaming = false,
  type = 'thinking'
}: ThoughtBubbleProps) {
  const icons = {
    thinking: '💭',
    tool: '🔧',
    decision: '🤔',
  };

  return (
    <AnimatePresence>
      {text && (
        <motion.div
          className={`thought-bubble thought-${position} thought-${type}`}
          initial={{ opacity: 0, scale: 0.8, x: -10 }}
          animate={{ opacity: 1, scale: 1, x: 0 }}
          exit={{ opacity: 0, scale: 0.8 }}
          transition={{ duration: 0.2 }}
        >
          <div className="thought-pointer" />
          <div className="thought-content">
            <span className="thought-icon">{icons[type]}</span>
            <span className="thought-text">
              {text}
              {streaming && <span className="cursor-blink">▊</span>}
            </span>
          </div>
        </motion.div>
      )}
    </AnimatePresence>
  );
}
```

**CSS Styles:**

```css
/* src/styles/thought-bubble.css */

.thought-bubble {
  position: absolute;
  background: linear-gradient(135deg, rgba(59, 130, 246, 0.95), rgba(37, 99, 235, 0.95));
  border-radius: 12px;
  padding: 8px 12px;
  max-width: 280px;
  font-size: 12px;
  color: white;
  box-shadow: 0 4px 20px rgba(0, 0, 0, 0.3);
  z-index: 1000;
  backdrop-filter: blur(8px);
}

.thought-right {
  left: calc(100% + 16px);
  top: 50%;
  transform: translateY(-50%);
}

.thought-top {
  bottom: calc(100% + 16px);
  left: 50%;
  transform: translateX(-50%);
}

.thought-pointer {
  position: absolute;
  width: 0;
  height: 0;
}

.thought-right .thought-pointer {
  left: -8px;
  top: 50%;
  transform: translateY(-50%);
  border: 8px solid transparent;
  border-right-color: rgba(59, 130, 246, 0.95);
}

.thought-top .thought-pointer {
  bottom: -8px;
  left: 50%;
  transform: translateX(-50%);
  border: 8px solid transparent;
  border-top-color: rgba(59, 130, 246, 0.95);
}

.thought-content {
  display: flex;
  align-items: flex-start;
  gap: 6px;
}

.thought-icon {
  flex-shrink: 0;
}

.thought-text {
  line-height: 1.4;
}

.thought-tool {
  background: linear-gradient(135deg, rgba(234, 179, 8, 0.95), rgba(202, 138, 4, 0.95));
}

.thought-decision {
  background: linear-gradient(135deg, rgba(168, 85, 247, 0.95), rgba(139, 92, 246, 0.95));
}

.cursor-blink {
  animation: blink 1s infinite;
  margin-left: 2px;
}

@keyframes blink {
  0%, 50% { opacity: 1; }
  51%, 100% { opacity: 0; }
}

/* Active node glow */
.node-active {
  box-shadow: 0 0 20px rgba(74, 222, 128, 0.6);
  border-color: #4ade80 !important;
}

.sub-agent-active {
  background: rgba(74, 222, 128, 0.3);
  border-color: #4ade80;
  box-shadow: 0 0 10px rgba(74, 222, 128, 0.4);
}
```


---

### 3. Hooks

#### useLayout.ts

```typescript
// src/hooks/useLayout.ts
import { useCallback } from 'react';
import { useReactFlow } from '@xyflow/react';
import dagre from 'dagre';

type LayoutDirection = 'TB' | 'LR';
type LayoutMode = 'pipeline' | 'tree' | 'cluster' | 'freeform';

interface LayoutOptions {
  direction?: LayoutDirection;
  nodeSpacing?: number;
  rankSpacing?: number;
}

export function useLayout() {
  const { getNodes, getEdges, setNodes, fitView } = useReactFlow();

  const detectLayoutMode = useCallback((): LayoutMode => {
    const nodes = getNodes();
    const edges = getEdges();
    
    // Analyze graph structure
    const hasContainers = nodes.some(n => 
      ['sequential', 'loop', 'parallel'].includes(n.type || '')
    );
    const hasRouter = nodes.some(n => n.type === 'router');
    const isLinear = edges.length === nodes.length - 1;
    
    if (isLinear && !hasRouter) return 'pipeline';
    if (hasRouter) return 'tree';
    if (hasContainers) return 'cluster';
    return 'freeform';
  }, [getNodes, getEdges]);

  const applyLayout = useCallback((options: LayoutOptions = {}) => {
    const nodes = getNodes();
    const edges = getEdges();
    
    if (nodes.length === 0) return;

    const mode = detectLayoutMode();
    const direction = options.direction || (mode === 'pipeline' ? 'LR' : 'TB');
    
    const g = new dagre.graphlib.Graph();
    g.setGraph({ 
      rankdir: direction,
      nodesep: options.nodeSpacing || 50,
      ranksep: options.rankSpacing || 80,
    });
    g.setDefaultEdgeLabel(() => ({}));

    nodes.forEach(node => {
      g.setNode(node.id, { 
        width: node.width || 180, 
        height: node.height || 100 
      });
    });

    edges.forEach(edge => {
      g.setEdge(edge.source, edge.target);
    });

    dagre.layout(g);

    const layoutedNodes = nodes.map(node => {
      const nodeWithPosition = g.node(node.id);
      return {
        ...node,
        position: {
          x: nodeWithPosition.x - (node.width || 180) / 2,
          y: nodeWithPosition.y - (node.height || 100) / 2,
        },
      };
    });

    setNodes(layoutedNodes);
    setTimeout(() => fitView({ padding: 0.2 }), 50);
  }, [getNodes, getEdges, setNodes, fitView, detectLayoutMode]);

  const fitToView = useCallback(() => {
    fitView({ padding: 0.2, duration: 300 });
  }, [fitView]);

  return { 
    applyLayout, 
    fitToView, 
    detectLayoutMode 
  };
}
```

#### useExecution.ts

```typescript
// src/hooks/useExecution.ts
import { useCallback, useState } from 'react';

interface ToolCall {
  id: string;
  name: string;
  args: unknown;
  result?: unknown;
  status: 'pending' | 'running' | 'complete' | 'error';
}

interface ExecutionState {
  isRunning: boolean;
  activeNode: string | null;
  activeSubAgent: string | null;
  thoughts: Record<string, string>;
  toolCalls: ToolCall[];
  iteration: number;
  startTime: number | null;
}

const initialState: ExecutionState = {
  isRunning: false,
  activeNode: null,
  activeSubAgent: null,
  thoughts: {},
  toolCalls: [],
  iteration: 0,
  startTime: null,
};

export function useExecution() {
  const [state, setState] = useState<ExecutionState>(initialState);

  const start = useCallback(() => {
    setState({
      ...initialState,
      isRunning: true,
      startTime: Date.now(),
    });
  }, []);

  const stop = useCallback(() => {
    setState(s => ({ ...s, isRunning: false }));
  }, []);

  const setActiveNode = useCallback((nodeId: string | null, subAgent?: string) => {
    setState(s => ({ 
      ...s, 
      activeNode: nodeId,
      activeSubAgent: subAgent || null,
    }));
  }, []);

  const setThought = useCallback((nodeId: string, thought: string) => {
    setState(s => ({
      ...s,
      thoughts: { ...s.thoughts, [nodeId]: thought },
    }));
  }, []);

  const clearThought = useCallback((nodeId: string) => {
    setState(s => {
      const { [nodeId]: _, ...rest } = s.thoughts;
      return { ...s, thoughts: rest };
    });
  }, []);

  const addToolCall = useCallback((toolCall: Omit<ToolCall, 'status'>) => {
    setState(s => ({
      ...s,
      toolCalls: [...s.toolCalls, { ...toolCall, status: 'running' }],
    }));
  }, []);

  const completeToolCall = useCallback((id: string, result: unknown) => {
    setState(s => ({
      ...s,
      toolCalls: s.toolCalls.map(tc =>
        tc.id === id ? { ...tc, result, status: 'complete' } : tc
      ),
    }));
  }, []);

  const incrementIteration = useCallback(() => {
    setState(s => ({ ...s, iteration: s.iteration + 1 }));
  }, []);

  const reset = useCallback(() => {
    setState(initialState);
  }, []);

  return {
    ...state,
    start,
    stop,
    setActiveNode,
    setThought,
    clearThought,
    addToolCall,
    completeToolCall,
    incrementIteration,
    reset,
  };
}
```

#### useNodeActions.ts

```typescript
// src/hooks/useNodeActions.ts
import { useCallback } from 'react';
import { useStore } from '../store';

export function useNodeActions() {
  const { addAgent, updateAgent, removeAgent, currentProject } = useStore();

  const createNode = useCallback((type: string, position?: { x: number; y: number }) => {
    const agentCount = Object.keys(currentProject?.agents || {}).length;
    const id = `${type}_${agentCount + 1}`;
    
    const defaults: Record<string, Partial<AgentSchema>> = {
      llm: {
        type: 'llm',
        model: 'gemini-2.0-flash',
        instruction: 'You are a helpful assistant.',
        tools: [],
      },
      sequential: {
        type: 'sequential',
        sub_agents: [],
      },
      loop: {
        type: 'loop',
        sub_agents: [],
        max_iterations: 3,
      },
      parallel: {
        type: 'parallel',
        sub_agents: [],
      },
      router: {
        type: 'router',
        model: 'gemini-2.0-flash',
        instruction: 'Classify the request.',
        routes: [],
      },
    };

    addAgent(id, {
      ...defaults[type],
      position: position || { x: 100, y: 100 },
    } as AgentSchema);

    return id;
  }, [addAgent, currentProject]);

  const duplicateNode = useCallback((nodeId: string) => {
    const agent = currentProject?.agents[nodeId];
    if (!agent) return null;

    const newId = `${nodeId}_copy`;
    addAgent(newId, {
      ...agent,
      position: {
        x: agent.position.x + 50,
        y: agent.position.y + 50,
      },
    });
    return newId;
  }, [addAgent, currentProject]);

  const deleteNode = useCallback((nodeId: string) => {
    removeAgent(nodeId);
  }, [removeAgent]);

  return {
    createNode,
    duplicateNode,
    deleteNode,
    updateNode: updateAgent,
  };
}
```

#### useKeyboardShortcuts.ts

```typescript
// src/hooks/useKeyboardShortcuts.ts
import { useEffect } from 'react';
import { useReactFlow } from '@xyflow/react';
import { useStore } from '../store';
import { useNodeActions } from './useNodeActions';
import { useLayout } from './useLayout';

export function useKeyboardShortcuts() {
  const { selectedNodeId, selectNode } = useStore();
  const { deleteNode, duplicateNode } = useNodeActions();
  const { applyLayout, fitToView } = useLayout();
  const { zoomIn, zoomOut } = useReactFlow();

  useEffect(() => {
    const handleKeyDown = (e: KeyboardEvent) => {
      // Ignore if typing in input
      if (['INPUT', 'TEXTAREA'].includes((e.target as HTMLElement).tagName)) {
        return;
      }

      const isMod = e.metaKey || e.ctrlKey;

      switch (true) {
        // Delete selected node
        case e.key === 'Delete' || e.key === 'Backspace':
          if (selectedNodeId) {
            deleteNode(selectedNodeId);
            selectNode(null);
          }
          break;

        // Duplicate: Cmd/Ctrl + D
        case isMod && e.key === 'd':
          e.preventDefault();
          if (selectedNodeId) {
            const newId = duplicateNode(selectedNodeId);
            if (newId) selectNode(newId);
          }
          break;

        // Auto-layout: Cmd/Ctrl + L
        case isMod && e.key === 'l':
          e.preventDefault();
          applyLayout();
          break;

        // Fit to view: Cmd/Ctrl + 0
        case isMod && e.key === '0':
          e.preventDefault();
          fitToView();
          break;

        // Zoom in: Cmd/Ctrl + =
        case isMod && (e.key === '=' || e.key === '+'):
          e.preventDefault();
          zoomIn();
          break;

        // Zoom out: Cmd/Ctrl + -
        case isMod && e.key === '-':
          e.preventDefault();
          zoomOut();
          break;

        // Deselect: Escape
        case e.key === 'Escape':
          selectNode(null);
          break;
      }
    };

    window.addEventListener('keydown', handleKeyDown);
    return () => window.removeEventListener('keydown', handleKeyDown);
  }, [selectedNodeId, deleteNode, duplicateNode, selectNode, applyLayout, fitToView, zoomIn, zoomOut]);
}
```

---

### 4. Layout System

#### analyzer.ts

```typescript
// src/layout/analyzer.ts

interface GraphAnalysis {
  nodeCount: number;
  edgeCount: number;
  maxDepth: number;
  hasCycles: boolean;
  hasContainers: boolean;
  hasRouter: boolean;
  dominantPattern: 'pipeline' | 'tree' | 'dag' | 'cyclic';
  clusters: string[][];
  entryPoints: string[];
  exitPoints: string[];
}

export function analyzeGraph(nodes: Node[], edges: Edge[]): GraphAnalysis {
  const nodeIds = new Set(nodes.map(n => n.id));
  const adjacency = new Map<string, string[]>();
  const inDegree = new Map<string, number>();
  
  // Build adjacency list
  nodes.forEach(n => {
    adjacency.set(n.id, []);
    inDegree.set(n.id, 0);
  });
  
  edges.forEach(e => {
    if (nodeIds.has(e.source) && nodeIds.has(e.target)) {
      adjacency.get(e.source)!.push(e.target);
      inDegree.set(e.target, (inDegree.get(e.target) || 0) + 1);
    }
  });

  // Find entry/exit points
  const entryPoints = nodes
    .filter(n => inDegree.get(n.id) === 0)
    .map(n => n.id);
  
  const exitPoints = nodes
    .filter(n => adjacency.get(n.id)!.length === 0)
    .map(n => n.id);

  // Detect cycles using DFS
  const hasCycles = detectCycles(adjacency, nodeIds);

  // Calculate max depth
  const maxDepth = calculateMaxDepth(adjacency, entryPoints);

  // Check for special node types
  const hasContainers = nodes.some(n => 
    ['sequential', 'loop', 'parallel'].includes(n.type || '')
  );
  const hasRouter = nodes.some(n => n.type === 'router');

  // Determine dominant pattern
  let dominantPattern: GraphAnalysis['dominantPattern'];
  if (hasCycles) {
    dominantPattern = 'cyclic';
  } else if (hasRouter || maxDepth > 3) {
    dominantPattern = 'tree';
  } else if (edges.length === nodes.length - 1 && entryPoints.length === 1) {
    dominantPattern = 'pipeline';
  } else {
    dominantPattern = 'dag';
  }

  return {
    nodeCount: nodes.length,
    edgeCount: edges.length,
    maxDepth,
    hasCycles,
    hasContainers,
    hasRouter,
    dominantPattern,
    clusters: [], // TODO: implement clustering
    entryPoints,
    exitPoints,
  };
}

function detectCycles(adjacency: Map<string, string[]>, nodeIds: Set<string>): boolean {
  const visited = new Set<string>();
  const recStack = new Set<string>();

  function dfs(node: string): boolean {
    visited.add(node);
    recStack.add(node);

    for (const neighbor of adjacency.get(node) || []) {
      if (!visited.has(neighbor)) {
        if (dfs(neighbor)) return true;
      } else if (recStack.has(neighbor)) {
        return true;
      }
    }

    recStack.delete(node);
    return false;
  }

  for (const node of nodeIds) {
    if (!visited.has(node)) {
      if (dfs(node)) return true;
    }
  }

  return false;
}

function calculateMaxDepth(adjacency: Map<string, string[]>, entryPoints: string[]): number {
  const depths = new Map<string, number>();
  
  function dfs(node: string, depth: number) {
    depths.set(node, Math.max(depths.get(node) || 0, depth));
    for (const neighbor of adjacency.get(node) || []) {
      dfs(neighbor, depth + 1);
    }
  }

  entryPoints.forEach(entry => dfs(entry, 0));
  
  return Math.max(...depths.values(), 0);
}
```

#### modes.ts

```typescript
// src/layout/modes.ts

export type LayoutMode = 'pipeline' | 'tree' | 'cluster' | 'radial' | 'freeform';

export interface LayoutConfig {
  direction: 'TB' | 'LR' | 'BT' | 'RL';
  nodeSpacing: number;
  rankSpacing: number;
  algorithm: 'dagre' | 'elk';
}

export const layoutPresets: Record<LayoutMode, LayoutConfig> = {
  pipeline: {
    direction: 'LR',
    nodeSpacing: 40,
    rankSpacing: 100,
    algorithm: 'dagre',
  },
  tree: {
    direction: 'TB',
    nodeSpacing: 50,
    rankSpacing: 80,
    algorithm: 'dagre',
  },
  cluster: {
    direction: 'TB',
    nodeSpacing: 30,
    rankSpacing: 60,
    algorithm: 'dagre',
  },
  radial: {
    direction: 'TB',
    nodeSpacing: 60,
    rankSpacing: 100,
    algorithm: 'elk', // Future
  },
  freeform: {
    direction: 'TB',
    nodeSpacing: 50,
    rankSpacing: 80,
    algorithm: 'dagre',
  },
};
```


---

### 5. Refactored Canvas

#### Canvas/index.tsx

```typescript
// src/components/Canvas/index.tsx
import { useCallback } from 'react';
import { ReactFlow, Background, Controls, MiniMap } from '@xyflow/react';
import '@xyflow/react/dist/style.css';

import { useStore } from '../../store';
import { useLayout } from '../../hooks/useLayout';
import { useExecution } from '../../hooks/useExecution';
import { useKeyboardShortcuts } from '../../hooks/useKeyboardShortcuts';

import { nodeTypes } from '../Nodes';
import { edgeTypes } from '../Edges';
import { CanvasToolbar } from './CanvasToolbar';
import { AgentPalette } from '../Panels/AgentPalette';
import { ToolPalette } from '../Panels/ToolPalette';
import { PropertiesPanel } from '../Panels/PropertiesPanel';
import { TestConsole } from '../Console/TestConsole';
import { MenuBar } from '../MenuBar';

export function Canvas() {
  const { 
    nodes, 
    edges, 
    onNodesChange, 
    onEdgesChange,
    selectedNodeId,
    selectNode,
  } = useStore();
  
  const { applyLayout, fitToView } = useLayout();
  const execution = useExecution();
  
  useKeyboardShortcuts();

  const onNodeClick = useCallback((_: React.MouseEvent, node: Node) => {
    selectNode(node.id);
  }, [selectNode]);

  const onPaneClick = useCallback(() => {
    selectNode(null);
  }, [selectNode]);

  // Inject execution state into nodes
  const nodesWithExecution = nodes.map(node => ({
    ...node,
    data: {
      ...node.data,
      isActive: execution.activeNode === node.id,
      activeSubAgent: execution.activeSubAgent,
      thought: execution.thoughts[node.id],
      iteration: execution.iteration,
    },
  }));

  return (
    <div className="flex flex-col h-full">
      <MenuBar />
      
      <div className="flex flex-1 overflow-hidden">
        {/* Left Sidebar */}
        <div className="w-48 bg-gray-900 border-r border-gray-700 flex flex-col">
          <AgentPalette />
          <ToolPalette />
        </div>

        {/* Main Canvas */}
        <div className="flex-1 relative">
          <CanvasToolbar 
            onAutoLayout={applyLayout}
            onFitView={fitToView}
          />
          
          <ReactFlow
            nodes={nodesWithExecution}
            edges={edges}
            nodeTypes={nodeTypes}
            edgeTypes={edgeTypes}
            onNodesChange={onNodesChange}
            onEdgesChange={onEdgesChange}
            onNodeClick={onNodeClick}
            onPaneClick={onPaneClick}
            fitView
          >
            <Background color="#333" gap={20} />
            <Controls />
            <MiniMap 
              nodeColor={(n) => n.data?.isActive ? '#4ade80' : '#666'}
              maskColor="rgba(0,0,0,0.8)"
            />
          </ReactFlow>
        </div>

        {/* Right Sidebar */}
        <div className="w-72 bg-gray-900 border-l border-gray-700">
          <PropertiesPanel />
        </div>
      </div>

      {/* Bottom Console */}
      <TestConsole execution={execution} />
    </div>
  );
}
```

#### CanvasToolbar.tsx

```typescript
// src/components/Canvas/CanvasToolbar.tsx

interface CanvasToolbarProps {
  onAutoLayout: () => void;
  onFitView: () => void;
}

export function CanvasToolbar({ onAutoLayout, onFitView }: CanvasToolbarProps) {
  return (
    <div className="absolute top-2 left-2 z-10 flex gap-2">
      <button
        onClick={onAutoLayout}
        className="px-3 py-1.5 bg-gray-800 hover:bg-gray-700 rounded text-sm flex items-center gap-2"
        title="Auto Layout (Ctrl+L)"
      >
        <span>⊞</span> Layout
      </button>
      
      <button
        onClick={onFitView}
        className="px-3 py-1.5 bg-gray-800 hover:bg-gray-700 rounded text-sm flex items-center gap-2"
        title="Fit to View (Ctrl+0)"
      >
        <span>⊡</span> Fit
      </button>
    </div>
  );
}
```

---

## Migration Plan

### Phase 1: Foundation (Day 1-2)

| Task | Files | Effort |
|------|-------|--------|
| Create folder structure | All directories | 30 min |
| Define TypeScript types | `types/*.ts` | 1 hr |
| Create BaseNode component | `Nodes/BaseNode.tsx` | 1 hr |
| Create ThoughtBubble | `Overlays/ThoughtBubble.tsx` | 1 hr |
| Add CSS styles | `styles/nodes.css`, `styles/thought-bubble.css` | 1 hr |

### Phase 2: Node Extraction (Day 3-4)

| Task | Files | Effort |
|------|-------|--------|
| Extract LlmAgentNode | `Nodes/LlmAgentNode.tsx` | 2 hr |
| Extract SequentialNode | `Nodes/SequentialNode.tsx` | 1 hr |
| Extract LoopNode | `Nodes/LoopNode.tsx` | 1 hr |
| Extract ParallelNode | `Nodes/ParallelNode.tsx` | 1 hr |
| Extract RouterNode | `Nodes/RouterNode.tsx` | 1 hr |
| Create node index | `Nodes/index.ts` | 30 min |
| Register with React Flow | Update Canvas | 1 hr |

### Phase 3: Hooks Extraction (Day 5-6)

| Task | Files | Effort |
|------|-------|--------|
| Create useLayout hook | `hooks/useLayout.ts` | 2 hr |
| Create useExecution hook | `hooks/useExecution.ts` | 2 hr |
| Create useNodeActions hook | `hooks/useNodeActions.ts` | 1 hr |
| Create useKeyboardShortcuts | `hooks/useKeyboardShortcuts.ts` | 1 hr |
| Install dagre | `npm install dagre @types/dagre` | 10 min |

### Phase 4: Panel Extraction (Day 7-8)

| Task | Files | Effort |
|------|-------|--------|
| Extract AgentPalette | `Panels/AgentPalette.tsx` | 2 hr |
| Extract ToolPalette | `Panels/ToolPalette.tsx` | 2 hr |
| Extract PropertiesPanel | `Panels/PropertiesPanel.tsx` | 3 hr |
| Update imports | All files | 1 hr |

### Phase 5: Canvas Refactor (Day 9-10)

| Task | Files | Effort |
|------|-------|--------|
| Create CanvasToolbar | `Canvas/CanvasToolbar.tsx` | 1 hr |
| Refactor main Canvas | `Canvas/index.tsx` | 3 hr |
| Wire up execution state | Canvas + useExecution | 2 hr |
| Test all functionality | Manual testing | 2 hr |

### Phase 6: Polish (Day 11-12)

| Task | Files | Effort |
|------|-------|--------|
| Add MiniMap | Canvas | 1 hr |
| Add edge animations | `Edges/AnimatedEdge.tsx` | 2 hr |
| Keyboard shortcuts | useKeyboardShortcuts | 1 hr |
| Bug fixes | Various | 4 hr |

---

## SSE Integration for Thoughts

Update `useSSE.ts` to emit thought events:

```typescript
// In useSSE.ts - add to trace event handler

es.addEventListener('trace', (e) => {
  const trace = JSON.parse(e.data);
  
  switch (trace.type) {
    case 'node_start':
      execution.setActiveNode(trace.node, trace.subAgent);
      break;
      
    case 'node_end':
      execution.clearThought(trace.node);
      break;
      
    case 'thinking':
      // New event type from backend
      execution.setThought(trace.node, trace.text);
      break;
      
    case 'tool_call':
      execution.addToolCall({
        id: trace.id,
        name: trace.name,
        args: trace.args,
      });
      execution.setThought(trace.node, `Calling ${trace.name}...`);
      break;
      
    case 'tool_result':
      execution.completeToolCall(trace.id, trace.result);
      execution.clearThought(trace.node);
      break;
      
    case 'iteration':
      execution.incrementIteration();
      break;
  }
});
```

Backend changes needed in `adk-studio/src/server/sse.rs`:
- Emit `thinking` events when LLM starts generating
- Emit `iteration` events for loop agents

---

## Dependencies to Add

```json
{
  "dependencies": {
    "dagre": "^0.8.5",
    "framer-motion": "^10.16.0"
  },
  "devDependencies": {
    "@types/dagre": "^0.7.52"
  }
}
```

---

## Success Criteria

| Criteria | Metric |
|----------|--------|
| Canvas.tsx reduced | < 200 lines |
| All node types extracted | 5 separate components |
| Thought bubbles working | Visible during execution |
| Auto-layout functional | One-click arrangement |
| Keyboard shortcuts | Delete, Duplicate, Layout, Fit |
| No regression | All existing features work |

---

## Future Enhancements

After this refactor, easier to add:

| Feature | Enabled By |
|---------|------------|
| Custom node shapes | Node component system |
| Semantic zoom | Node data structure |
| State inspector | useExecution hook |
| Undo/Redo | Centralized actions |
| ELK layout | Layout system abstraction |
| Node animations | Framer Motion integration |

---

## File Size Targets

| File | Current | Target |
|------|---------|--------|
| Canvas/index.tsx | 1500+ | < 200 |
| Nodes/LlmAgentNode.tsx | - | < 80 |
| Nodes/SequentialNode.tsx | - | < 60 |
| hooks/useLayout.ts | - | < 100 |
| hooks/useExecution.ts | - | < 80 |
| Panels/PropertiesPanel.tsx | - | < 300 |

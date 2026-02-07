import { useCallback, useState, useMemo, useRef } from 'react';
import type { StateSnapshot, InterruptData } from '../types/execution';
import { useExecutionPath } from './useExecutionPath';

/**
 * Flow phase for edge animations.
 * - 'idle': No activity
 * - 'trigger_input': User submitting input to trigger (animates trigger→START)
 * - 'input': Data flowing from START to agents
 * - 'output': Agent generating response
 * - 'interrupted': Waiting for HITL response
 * @see trigger-input-flow Requirements 2.2, 2.3, 3.1, 3.2
 */
export type FlowPhase = 'idle' | 'trigger_input' | 'input' | 'output' | 'interrupted';

/**
 * Hook that manages all execution-related state for the Canvas:
 * - Flow phase, active agent, iteration, thoughts
 * - Execution path tracking
 * - Timeline & state inspector (snapshots, scrubbing)
 * - Data flow overlay (state keys, highlighting)
 * - HITL interrupt state
 *
 * @see Requirements 2.1, 2.4: Canvas delegates execution state management
 */
export function useCanvasExecution(deps: {
  showDataFlowOverlay: boolean;
  setShowDataFlowOverlay: (v: boolean) => void;
}) {
  const { showDataFlowOverlay, setShowDataFlowOverlay } = deps;

  // Execution state
  const [flowPhase, setFlowPhase] = useState<FlowPhase>('idle');
  const [activeAgent, setActiveAgent] = useState<string | null>(null);
  const [iteration, setIteration] = useState(0);
  const [thoughts, setThoughts] = useState<Record<string, string>>({});

  // Execution path tracking (v2.0)
  // @see Requirements 10.3, 10.5: Execution path highlighting
  const executionPath = useExecutionPath();

  // Keep a ref to executionPath so setTimeout callbacks read fresh state
  const execPathRef = useRef(executionPath);
  execPathRef.current = executionPath;

  // Timeline state (v2.0)
  const [timelineCollapsed, setTimelineCollapsed] = useState(false);
  const [snapshots, setSnapshots] = useState<StateSnapshot[]>([]);
  const [currentSnapshotIndex, setCurrentSnapshotIndex] = useState(-1);
  const [scrubToFn, setScrubToFn] = useState<((index: number) => void) | null>(null);

  // State Inspector visibility (v2.0)
  const [showStateInspector, setShowStateInspector] = useState(true);

  // HITL: Interrupted node ID for visual indicator (v2.0)
  // @see trigger-input-flow Requirement 3.3: Interrupt visual indicator
  const [interruptedNodeId, setInterruptedNodeId] = useState<string | null>(null);

  // Data Flow Overlay state (v2.0)
  // @see Requirements 3.1-3.9: Data flow overlays
  const [stateKeys, setStateKeys] = useState<Map<string, string[]>>(new Map());
  const [highlightedKey, setHighlightedKey] = useState<string | null>(null);

  // v2.0: Queue refs for active agent animation (declared before handlers that use them)
  const activeAgentQueueRef = useRef<string[]>([]);
  const processingRef = useRef(false);
  const currentDisplayedAgentRef = useRef<string | null>(null);
  const pendingIdleRef = useRef(false);
  const pendingCompleteRef = useRef(false);

  // v2.0: Wrapper for flow phase that also updates execution path
  // Start execution on trigger_input (not input) so the path is ready
  // BEFORE trace events arrive — action nodes complete in <1ms.
  // @see Requirements 10.3, 10.5: Execution path highlighting
  const handleFlowPhase = useCallback((phase: FlowPhase) => {
    setFlowPhase(phase);
    const ep = execPathRef.current;
    if (phase === 'trigger_input') {
      // Reset queue state for new execution
      activeAgentQueueRef.current = [];
      processingRef.current = false;
      pendingIdleRef.current = false;
      pendingCompleteRef.current = false;
      currentDisplayedAgentRef.current = null;
      ep.startExecution();
    } else if (phase === 'idle' && ep.isExecuting) {
      // Don't complete execution immediately — the animation queue may still
      // be draining (action nodes queue with 300ms display each). Mark pending
      // and let processAgentQueue handle completion when the queue is empty.
      if (processingRef.current || activeAgentQueueRef.current.length > 0) {
        pendingCompleteRef.current = true;
      } else {
        ep.completeExecution();
      }
    }
  }, []);

  // v2.0: Wrapper for active agent that also updates execution path.
  // Uses a queue + minimum display duration so fast action nodes (0ms)
  // still get visible edge animation (~300ms each).

  const processAgentQueue = useCallback(() => {
    if (processingRef.current) return;
    const next = activeAgentQueueRef.current.shift();
    if (!next) {
      // Queue empty — if execution ended while we were draining, clear now
      if (pendingIdleRef.current) {
        pendingIdleRef.current = false;
        currentDisplayedAgentRef.current = null;
        setActiveAgent(null);
      }
      // If completeExecution was deferred, run it now that the queue is drained
      if (pendingCompleteRef.current) {
        pendingCompleteRef.current = false;
        const ep = execPathRef.current;
        if (ep.isExecuting) {
          ep.completeExecution();
        }
      }
      return;
    }

    processingRef.current = true;
    currentDisplayedAgentRef.current = next;
    setActiveAgent(next);
    const ep = execPathRef.current;
    if (ep.isExecuting && !ep.path.includes(next)) {
      ep.moveToNode(next);
    }

    // Hold this node as "active" for at least 300ms so the edge animation is visible
    setTimeout(() => {
      processingRef.current = false;
      processAgentQueue(); // Process next item or handle pending idle
    }, 300);
  }, []);

  const handleActiveAgent = useCallback((agent: string | null) => {
    if (!agent) {
      // Execution ended — mark pending idle, queue will clear when drained
      pendingIdleRef.current = true;
      if (!processingRef.current && activeAgentQueueRef.current.length === 0) {
        // Nothing in flight — clear immediately
        pendingIdleRef.current = false;
        currentDisplayedAgentRef.current = null;
        setActiveAgent(null);
      }
      return;
    }

    pendingIdleRef.current = false;
    // Skip if this exact agent is currently being displayed (avoid double-queue
    // from rapid duplicate events). But DO allow re-queuing nodes that are
    // already in the execution path — this is essential for loop iterations
    // where the same node (e.g. loop_items, transform_prompt) executes
    // multiple times and each iteration needs edge animation.
    if (currentDisplayedAgentRef.current === agent) return;
    // Also skip if the same agent is already the last item in the queue
    // (prevents rapid duplicate queueing within the same iteration)
    const queue = activeAgentQueueRef.current;
    if (queue.length > 0 && queue[queue.length - 1] === agent) return;

    queue.push(agent);
    processAgentQueue();
  }, [processAgentQueue]);

  // Thought bubble handler
  const handleThought = useCallback((agent: string, thought: string | null) => {
    setThoughts(prev =>
      thought
        ? { ...prev, [agent]: thought }
        : Object.fromEntries(Object.entries(prev).filter(([k]) => k !== agent))
    );
  }, []);

  // Handler for state key hover (for highlighting related edges)
  // @see Requirements 3.8: Highlight all edges using same key on hover
  const handleKeyHover = useCallback((key: string | null) => {
    setHighlightedKey(key);
  }, []);

  // Handler for toggling data flow overlay
  // @see Requirements 3.4: Toggle to show/hide data flow overlays
  const handleToggleDataFlowOverlay = useCallback(() => {
    setShowDataFlowOverlay(!showDataFlowOverlay);
  }, [showDataFlowOverlay, setShowDataFlowOverlay]);

  // HITL: Handler for interrupt state changes from TestConsole
  // @see trigger-input-flow Requirement 3.3: Interrupt visual indicator
  const handleInterruptChange = useCallback((interrupt: InterruptData | null) => {
    setInterruptedNodeId(interrupt?.nodeId || null);
  }, []);

  // Handler for receiving snapshots and state keys from TestConsole
  const handleSnapshotsChange = useCallback((
    newSnapshots: StateSnapshot[],
    newIndex: number,
    scrubTo: (index: number) => void,
    newStateKeys?: Map<string, string[]>
  ) => {
    setSnapshots(newSnapshots);
    setCurrentSnapshotIndex(newIndex);
    setScrubToFn(() => scrubTo);
    if (newStateKeys) {
      setStateKeys(newStateKeys);
    }

    // NOTE: Do NOT rebuild the execution path here during live execution.
    // The execution path is managed by handleFlowPhase (startExecution) and
    // handleActiveAgent (moveToNode via the queue). Rebuilding here races with
    // the queue and causes handleActiveAgent to skip nodes (because they're
    // already in the path), which breaks edge animation.
    // The path is only rebuilt from snapshots during timeline scrubbing
    // (see handleStateHistorySelect).
  }, []);

  // Current and previous snapshots for StateInspector (v2.0)
  // @see Requirements 4.5, 5.4: Update inspector when timeline position changes
  const currentSnapshot = useMemo(() => {
    if (currentSnapshotIndex < 0 || currentSnapshotIndex >= snapshots.length) {
      return null;
    }
    return snapshots[currentSnapshotIndex];
  }, [snapshots, currentSnapshotIndex]);

  const previousSnapshot = useMemo(() => {
    const prevIndex = currentSnapshotIndex - 1;
    if (prevIndex < 0 || prevIndex >= snapshots.length) {
      return null;
    }
    return snapshots[prevIndex];
  }, [snapshots, currentSnapshotIndex]);

  // Handler for state inspector history selection
  const handleStateHistorySelect = useCallback((index: number) => {
    if (scrubToFn) {
      scrubToFn(index);
    }
  }, [scrubToFn]);

  return {
    // Execution state
    flowPhase,
    activeAgent,
    iteration,
    setIteration,
    thoughts,
    executionPath,

    // Timeline state
    timelineCollapsed,
    setTimelineCollapsed,
    snapshots,
    currentSnapshotIndex,
    scrubToFn,

    // State inspector
    showStateInspector,
    setShowStateInspector,
    currentSnapshot,
    previousSnapshot,

    // HITL
    interruptedNodeId,

    // Data flow overlay
    stateKeys,
    highlightedKey,

    // Handlers
    handleFlowPhase,
    handleActiveAgent,
    handleThought,
    handleKeyHover,
    handleToggleDataFlowOverlay,
    handleInterruptChange,
    handleSnapshotsChange,
    handleStateHistorySelect,
  };
}

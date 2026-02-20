import { useCallback, useRef, useState } from 'react';
import type { InterruptData, StateSnapshot, TraceEventPayload } from '../types/execution';
import type { DebugEntry } from '../types/debug';
import { MAX_DEBUG_ENTRIES } from '../types/debug';

/**
 * Flow phase for edge animations.
 * @see trigger-input-flow Requirements 2.2, 2.3, 3.1, 3.2
 */
export type FlowPhase = 'idle' | 'trigger_input' | 'input' | 'output' | 'interrupted';

interface ToolCall {
  name: string;
  args: unknown;
}

export interface TraceEvent {
  type: 'user' | 'agent_start' | 'agent_end' | 'model' | 'tool_call' | 'tool_result' | 'done' | 'error' | 'interrupt' | 'resume';
  timestamp: number;
  data: string;
  agent?: string;
  screenshot?: string; // base64 image for browser screenshots
  /** Interrupt data for HITL events */
  interruptData?: InterruptData;
}

/**
 * Parse a trace event payload from SSE v2.0 format.
 * Extracts state_snapshot and state_keys for timeline/data flow features.
 */
function parseTracePayload(data: string): TraceEventPayload | null {
  try {
    return JSON.parse(data) as TraceEventPayload;
  } catch {
    return null;
  }
}

/**
 * Convert a trace event payload to a StateSnapshot for timeline debugging.
 * 
 * @see Requirements 5.8: State snapshot capture
 */
function traceToSnapshot(
  trace: TraceEventPayload,
  nodeId: string,
  status: 'running' | 'success' | 'error'
): StateSnapshot | null {
  if (!trace.state_snapshot) {
    return null;
  }
  
  return {
    nodeId,
    timestamp: Date.now(),
    inputState: trace.state_snapshot.input || {},
    outputState: trace.state_snapshot.output || {},
    duration: trace.duration_ms || 0,
    status,
  };
}

export function useSSE(projectId: string | null, binaryPath?: string | null) {
  const [isStreaming, setIsStreaming] = useState(false);
  const [streamingText, setStreamingText] = useState('');
  const [currentAgent, setCurrentAgent] = useState('');
  const [toolCalls, setToolCalls] = useState<ToolCall[]>([]);
  const [events, setEvents] = useState<TraceEvent[]>([]);
  const [sessionId, setSessionId] = useState<string | null>(null);
  const [iteration, setIteration] = useState(0);
  
  // Ref-based queue of node_start events for edge animation.
  // React batches state updates, so rapid node_start events (action nodes
  // completing in <1ms) would collapse into a single currentAgent update.
  // This ref-based queue preserves every node_start. A state counter
  // triggers the consumer effect without causing infinite loops.
  const nodeStartQueueRef = useRef<string[]>([]);
  const [nodeStartTick, setNodeStartTick] = useState(0);
  
  // v2.0: State snapshots for timeline debugging
  const [snapshots, setSnapshots] = useState<StateSnapshot[]>([]);
  const [currentSnapshotIndex, setCurrentSnapshotIndex] = useState(-1);
  
  // v2.0: State keys for data flow overlays (edge ID -> state keys)
  const [stateKeys, setStateKeys] = useState<Map<string, string[]>>(new Map());
  
  // Debug console: debug entries from SSE debug events
  // @see debug-console-tab Requirements 10.1, 10.2, 10.3
  const [debugEntries, setDebugEntries] = useState<DebugEntry[]>([]);
  const debugIdCounter = useRef(0);

  // HITL: Flow phase for edge animations
  // @see trigger-input-flow Requirements 2.2, 2.3, 3.1, 3.2
  const [flowPhase, setFlowPhase] = useState<FlowPhase>('idle');
  
  // HITL: Interrupt state for human-in-the-loop interactions
  // @see trigger-input-flow Requirements 3.1, 3.2
  const [interrupt, setInterrupt] = useState<InterruptData | null>(null);
  
  const esRef = useRef<EventSource | null>(null);
  const textRef = useRef('');
  const agentRef = useRef('');
  const sessionRef = useRef<string | null>(null);
  const iterRef = useRef(0);
  const seenAgentsRef = useRef<Set<string>>(new Set());

  const addEvent = (type: TraceEvent['type'], data: string, agent?: string, screenshot?: string) => {
    setEvents(prev => [...prev, { type, timestamp: Date.now(), data, agent: agent || agentRef.current, screenshot }]);
  };

  /**
   * Add a state snapshot for timeline debugging (v2.0).
   * Maintains a maximum of 100 entries (best-effort retention).
   * 
   * @see Requirements 5.8: State snapshot capture
   */
  const addSnapshot = useCallback((snapshot: StateSnapshot) => {
    setSnapshots(prev => {
      const newSnapshots = [...prev, snapshot];
      const maxSnapshots = 100; // MAX_SNAPSHOTS
      if (newSnapshots.length > maxSnapshots) {
        newSnapshots.shift(); // Remove oldest
      }
      return newSnapshots;
    });
    setCurrentSnapshotIndex(prev => prev + 1);
  }, []);

  /**
   * Update state keys for a node (for data flow overlays).
   * 
   * @see Requirements 3.3: State keys from runtime events
   */
  const updateStateKeys = useCallback((nodeId: string, keys: string[]) => {
    setStateKeys(prev => {
      const newMap = new Map(prev);
      newMap.set(nodeId, keys);
      return newMap;
    });
  }, []);

  /**
   * Scrub to a specific position in the timeline.
   * 
   * @see Requirements 5.3, 5.4: Timeline scrubbing
   */
  const scrubTo = useCallback((index: number) => {
    setCurrentSnapshotIndex(Math.max(0, Math.min(index, snapshots.length - 1)));
  }, [snapshots.length]);

  const send = useCallback(
    (input: string, onComplete: (text: string) => void, onError?: (msg: string) => void, overrideSessionId?: string) => {
      if (!projectId) return;

      textRef.current = '';
      agentRef.current = '';
      iterRef.current = 0;
      seenAgentsRef.current = new Set();
      setStreamingText('');
      setCurrentAgent('');
      setToolCalls([]);
      setIteration(0);
      nodeStartQueueRef.current = [];
      // v2.0: Reset snapshots and state keys for new execution
      setSnapshots([]);
      setCurrentSnapshotIndex(-1);
      setStateKeys(new Map());
      // HITL: Reset interrupt state and set flow phase to trigger_input
      // @see trigger-input-flow Requirements 2.2, 3.1, 3.2
      setInterrupt(null);
      setFlowPhase('trigger_input');
      // Transition from trigger_input to input phase after animation
      setTimeout(() => setFlowPhase('input'), 500);
      // Append new user event, don't clear history
      setEvents(prev => [...prev, { type: 'user', timestamp: Date.now(), data: input }]);
      setIsStreaming(true);

      const params = new URLSearchParams({ input });
      if (binaryPath) {
        params.set('binary_path', binaryPath);
      }
      // Pass session ID - use override if provided (for webhooks), otherwise use existing session
      const sessionToUse = overrideSessionId || sessionRef.current;
      if (sessionToUse) {
        params.set('session_id', sessionToUse);
      }
      const es = new EventSource(`/api/projects/${projectId}/stream?${params}`);
      esRef.current = es;
      let ended = false;

      es.addEventListener('session', (e) => {
        sessionRef.current = e.data;
        setSessionId(e.data);
      });

      es.addEventListener('agent', (e) => {
        if (textRef.current) {
          textRef.current += '\n\n';
          setStreamingText(textRef.current);
        }
        agentRef.current = e.data;
        setCurrentAgent(e.data);
        addEvent('agent_start', 'runtime', e.data);
      });

      es.addEventListener('chunk', (e) => {
        textRef.current = e.data;  // Replace, not append (binary sends full response)
        setStreamingText(textRef.current);
      });

      es.addEventListener('trace', (e) => {
        const trace = parseTracePayload(e.data);
        if (!trace) return;

        if (trace.type === 'node_start') {
          const node = trace.node || '';
          // Track iterations: if we see an agent we've seen before, increment iteration
          if (seenAgentsRef.current.has(node)) {
            iterRef.current++;
            setIteration(iterRef.current);
            seenAgentsRef.current.clear();
          }
          seenAgentsRef.current.add(node);
          agentRef.current = node;
          setCurrentAgent(node);
          // Push to node start queue so the animation system sees every node,
          // even when React batches rapid state updates from fast action nodes.
          nodeStartQueueRef.current.push(node);
          setNodeStartTick(t => t + 1);
          addEvent('agent_start', `Iter ${iterRef.current + 1}, Step ${trace.step}`, node);
          
          // When a new node starts (e.g. loop iteration 2+), reset flowPhase
          // back to 'input' so edge animations fire. Without this, the phase
          // stays 'output' from the previous LLM response and the edge
          // animation check (flowPhase !== 'output') suppresses all animations.
          //
          // CRITICAL: Also clear streamingText so the TestConsole effect
          // `if (streamingText) { onFlowPhase?.('output') }` doesn't
          // immediately race and override this back to 'output'.
          textRef.current = '';
          setStreamingText('');
          setFlowPhase('input');
          
          // v2.0: Don't capture snapshot at node_start - wait for node_end
          // This avoids showing "running" spinners that never update
          
          // v2.0: Update state keys for data flow overlays
          if (trace.state_keys && trace.state_keys.length > 0) {
            updateStateKeys(node, trace.state_keys);
          }
        } else if (trace.type === 'node_end') {
          const node = trace.node || '';
          addEvent('agent_end', `${trace.duration_ms}ms`, node);
          
          // v2.0: Capture state snapshot at node end (with complete input/output)
          const snapshot = traceToSnapshot(trace, node, 'success');
          if (snapshot) {
            snapshot.step = trace.step;
            addSnapshot(snapshot);
          }
          
          // v2.0: Update state keys for data flow overlays
          if (trace.state_keys && trace.state_keys.length > 0) {
            updateStateKeys(node, trace.state_keys);
          }
        } else if (trace.type === 'state') {
          const state = trace.state || trace.state_snapshot?.output || {};
          if (state.response) {
            const response = typeof state.response === 'string' ? state.response : JSON.stringify(state.response);
            addEvent('model', response.slice(0, 100) + (response.length > 100 ? '...' : ''), agentRef.current);
          }
        } else if (trace.type === 'done') {
          const state = trace.state || trace.state_snapshot?.output || {};
          if (state.response) {
            const response = typeof state.response === 'string' ? state.response : JSON.stringify(state.response);
            addEvent('model', response.slice(0, 150) + (response.length > 150 ? '...' : ''));
          }
          addEvent('done', `${trace.total_steps} steps`);
          
          // v2.0: The done handler previously updated the last snapshot with
          // the final output state to fix a timing issue. This is no longer
          // needed because:
          // 1. LLM agents emit node_end from the message handler (with response)
          // 2. Action nodes emit node_end from emit_pending_node_ends (with
          //    reconstructed output from the project config)
          // Overwriting would clobber the per-node output with the full state.
        }
      });

      es.addEventListener('log', (e) => {
        try {
          const data = JSON.parse(e.data);
          if (data.agent) {
            agentRef.current = data.agent;
            setCurrentAgent(data.agent);
          }
          if (data.message) {
            addEvent('model', data.message, data.agent);
          }
        } catch (err) { console.warn('Failed to parse log event:', err); }
      });

      es.addEventListener('tool_call', (e) => {
        try {
          const data = JSON.parse(e.data);
          setToolCalls(prev => [...prev, { name: data.name, args: data.args }]);
          textRef.current += `\n🔧 Calling ${data.name}...\n`;
          setStreamingText(textRef.current);
          addEvent('tool_call', `${data.name}(${JSON.stringify(data.args)})`);
        } catch (err) { console.warn('Failed to parse tool_call event:', err); }
      });

      es.addEventListener('tool_result', (e) => {
        try {
          const data = JSON.parse(e.data);
          const result = typeof data.result === 'string' ? JSON.parse(data.result) : data.result;
          
          // Check for screenshot (base64 image)
          let screenshot: string | undefined;
          if (result?.base64_image) {
            screenshot = result.base64_image;
          }
          
          const resultStr = screenshot ? '📸 Screenshot captured' : 
            (typeof data.result === 'string' ? data.result : JSON.stringify(data.result).slice(0, 200));
          textRef.current += `✓ ${data.name}: ${resultStr}\n`;
          setStreamingText(textRef.current);
          addEvent('tool_result', `${data.name} → ${resultStr}`, undefined, screenshot);
        } catch (err) { console.warn('Failed to parse tool_result event:', err); }
      });

      /**
       * Handle interrupt event from ADK-Graph HITL.
       * Sets the flow phase to 'interrupted' and stores interrupt data.
       * @see trigger-input-flow Requirements 3.1: Interrupt detection
       */
      es.addEventListener('interrupt', (e) => {
        try {
          const data = JSON.parse(e.data);
          const interruptData: InterruptData = {
            nodeId: data.node_id || data.nodeId || '',
            message: data.message || 'Human input required',
            data: data.data || {},
          };
          
          // Set flow phase to interrupted
          setFlowPhase('interrupted');
          
          // Store interrupt data for UI display
          setInterrupt(interruptData);
          
          // Add interrupt event to trace history
          setEvents(prev => [...prev, {
            type: 'interrupt',
            timestamp: Date.now(),
            data: interruptData.message,
            agent: interruptData.nodeId,
            interruptData,
          }]);
        } catch (err) { console.warn('Failed to parse interrupt event:', err); }
      });

      /**
       * Handle resume event after user responds to interrupt.
       * Clears interrupt state and resumes normal flow.
       * @see trigger-input-flow Requirements 3.2: Interrupt response
       */
      es.addEventListener('resume', (e) => {
        try {
          const data = JSON.parse(e.data);
          const nodeId = data.node_id || data.nodeId || '';
          
          // Clear interrupt state
          setInterrupt(null);
          
          // Resume normal flow phase
          setFlowPhase('input');
          
          // Add resume event to trace history
          setEvents(prev => [...prev, {
            type: 'resume',
            timestamp: Date.now(),
            data: `Resumed from ${nodeId}`,
            agent: nodeId,
          }]);
        } catch (err) { console.warn('Failed to parse resume event:', err); }
      });

      /**
       * Handle debug SSE events for the Debug Console tab.
       * Parses the event payload into a DebugEntry and appends to the store,
       * capping at MAX_DEBUG_ENTRIES (500) by discarding oldest entries.
       * @see debug-console-tab Requirements 10.1, 10.2
       */
      es.addEventListener('debug', (e) => {
        try {
          const data = JSON.parse(e.data);
          const entry: DebugEntry = {
            id: `debug-${++debugIdCounter.current}`,
            timestamp: data.timestamp ?? Date.now(),
            level: data.level ?? 'debug',
            category: data.category ?? 'lifecycle',
            agent: data.agent ?? '',
            summary: data.summary ?? '',
            detail: data.detail ?? null,
          };
          setDebugEntries(prev => {
            const next = [...prev, entry];
            if (next.length > MAX_DEBUG_ENTRIES) {
              return next.slice(next.length - MAX_DEBUG_ENTRIES);
            }
            return next;
          });
        } catch (err) {
          console.warn('Failed to parse debug event:', err);
        }
      });

      es.addEventListener('end', () => {
        ended = true;
        const finalText = textRef.current;
        setStreamingText('');
        setCurrentAgent('');
        setIsStreaming(false);
        // HITL: Reset flow phase on completion
        setFlowPhase('idle');
        es.close();
        onComplete(finalText);
      });

      es.addEventListener('error', (e) => {
        if (!ended) {
          const msg = (e as MessageEvent).data || 'Connection error';
          setStreamingText('');
          setCurrentAgent('');
          setIsStreaming(false);
          // HITL: Reset flow phase on error
          setFlowPhase('idle');
          setInterrupt(null);
          es.close();
          addEvent('error', msg);
          onError?.(msg);
        }
      });
    },
    [projectId, binaryPath, addSnapshot, updateStateKeys]
  );

  const cancel = useCallback(() => {
    esRef.current?.close();
    setStreamingText('');
    setCurrentAgent('');
    setIsStreaming(false);
    // HITL: Reset flow phase and interrupt state on cancel
    setFlowPhase('idle');
    setInterrupt(null);
    
    // Also kill the backend session to stop the running process
    if (sessionRef.current) {
      fetch(`/api/sessions/${sessionRef.current}`, { method: 'DELETE' }).catch(() => {});
    }
  }, []);

  const clearEvents = useCallback(() => setEvents([]), []);

  const clearDebugEntries = useCallback(() => setDebugEntries([]), []);

  const newSession = useCallback(async () => {
    // Kill the old session process on the server
    if (sessionRef.current) {
      await fetch(`/api/sessions/${sessionRef.current}`, { method: 'DELETE' }).catch(() => {});
    }
    sessionRef.current = null;
    setSessionId(null);
    setEvents([]);
    // v2.0: Clear snapshots and state keys
    setSnapshots([]);
    setCurrentSnapshotIndex(-1);
    setStateKeys(new Map());
    // Debug console: Clear debug entries on new session
    // @see debug-console-tab Requirement 10.3
    setDebugEntries([]);
    // HITL: Clear interrupt state and flow phase
    setInterrupt(null);
    setFlowPhase('idle');
  }, []);

  return {
    send,
    cancel,
    isStreaming,
    streamingText,
    currentAgent,
    toolCalls,
    events,
    clearEvents,
    sessionId,
    newSession,
    iteration,
    // v2.0: State snapshot and data flow overlay support
    snapshots,
    currentSnapshotIndex,
    scrubTo,
    stateKeys,
    // HITL: Flow phase and interrupt state
    // @see trigger-input-flow Requirements 3.1, 3.2
    flowPhase,
    setFlowPhase,
    interrupt,
    setInterrupt,
    // Debug console: debug entries and clear callback
    // @see debug-console-tab Requirements 10.1, 10.2, 10.3
    debugEntries,
    clearDebugEntries,
    // Edge animation: queue of node_start events (preserves rapid-fire action nodes)
    nodeStartQueueRef,
    nodeStartTick,
  };
}

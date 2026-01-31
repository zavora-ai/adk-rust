import { useCallback, useRef, useState } from 'react';
import type { StateSnapshot, TraceEventPayload } from '../types/execution';

interface ToolCall {
  name: string;
  args: unknown;
}

export interface TraceEvent {
  type: 'user' | 'agent_start' | 'agent_end' | 'model' | 'tool_call' | 'tool_result' | 'done' | 'error';
  timestamp: number;
  data: string;
  agent?: string;
  screenshot?: string; // base64 image for browser screenshots
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
  
  // v2.0: State snapshots for timeline debugging
  const [snapshots, setSnapshots] = useState<StateSnapshot[]>([]);
  const [currentSnapshotIndex, setCurrentSnapshotIndex] = useState(-1);
  
  // v2.0: State keys for data flow overlays (edge ID -> state keys)
  const [stateKeys, setStateKeys] = useState<Map<string, string[]>>(new Map());
  
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
    (input: string, onComplete: (text: string) => void, onError?: (msg: string) => void) => {
      if (!projectId) return;

      textRef.current = '';
      agentRef.current = '';
      iterRef.current = 0;
      seenAgentsRef.current = new Set();
      setStreamingText('');
      setCurrentAgent('');
      setToolCalls([]);
      setIteration(0);
      // v2.0: Reset snapshots and state keys for new execution
      setSnapshots([]);
      setCurrentSnapshotIndex(-1);
      setStateKeys(new Map());
      // Append new user event, don't clear history
      setEvents(prev => [...prev, { type: 'user', timestamp: Date.now(), data: input }]);
      setIsStreaming(true);

      const params = new URLSearchParams({ input });
      if (binaryPath) {
        params.set('binary_path', binaryPath);
      }
      // Pass session ID if we have one
      if (sessionRef.current) {
        params.set('session_id', sessionRef.current);
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
          addEvent('agent_start', `Iter ${iterRef.current + 1}, Step ${trace.step}`, node);
          
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
          
          // v2.0: Update the last agent snapshot with the final output state
          // This fixes the timing issue where node_end fires before response is captured
          if (trace.state_snapshot) {
            setSnapshots(prev => {
              if (prev.length === 0) return prev;
              
              // Find the last non-done snapshot and update its output state
              const updated = [...prev];
              const lastIdx = updated.length - 1;
              if (updated[lastIdx] && updated[lastIdx].nodeId !== '__done__') {
                updated[lastIdx] = {
                  ...updated[lastIdx],
                  outputState: trace.state_snapshot?.output || updated[lastIdx].outputState,
                };
              }
              return updated;
            });
          }
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
        } catch {}
      });

      es.addEventListener('tool_call', (e) => {
        try {
          const data = JSON.parse(e.data);
          setToolCalls(prev => [...prev, { name: data.name, args: data.args }]);
          textRef.current += `\n🔧 Calling ${data.name}...\n`;
          setStreamingText(textRef.current);
          addEvent('tool_call', `${data.name}(${JSON.stringify(data.args)})`);
        } catch {}
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
        } catch {}
      });

      es.addEventListener('end', () => {
        ended = true;
        const finalText = textRef.current;
        setStreamingText('');
        setCurrentAgent('');
        setIsStreaming(false);
        es.close();
        onComplete(finalText);
      });

      es.addEventListener('error', (e) => {
        if (!ended) {
          const msg = (e as MessageEvent).data || 'Connection error';
          setStreamingText('');
          setCurrentAgent('');
          setIsStreaming(false);
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
    
    // Also kill the backend session to stop the running process
    if (sessionRef.current) {
      fetch(`/api/sessions/${sessionRef.current}`, { method: 'DELETE' }).catch(() => {});
    }
  }, []);

  const clearEvents = useCallback(() => setEvents([]), []);

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
  };
}

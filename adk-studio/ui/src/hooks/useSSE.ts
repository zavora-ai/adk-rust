import { useCallback, useRef, useState } from 'react';

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

export function useSSE(projectId: string | null, binaryPath?: string | null) {
  const [isStreaming, setIsStreaming] = useState(false);
  const [streamingText, setStreamingText] = useState('');
  const [currentAgent, setCurrentAgent] = useState('');
  const [toolCalls, setToolCalls] = useState<ToolCall[]>([]);
  const [events, setEvents] = useState<TraceEvent[]>([]);
  const [sessionId, setSessionId] = useState<string | null>(null);
  const [iteration, setIteration] = useState(0);
  const esRef = useRef<EventSource | null>(null);
  const textRef = useRef('');
  const agentRef = useRef('');
  const sessionRef = useRef<string | null>(null);
  const iterRef = useRef(0);
  const seenAgentsRef = useRef<Set<string>>(new Set());

  const addEvent = (type: TraceEvent['type'], data: string, agent?: string, screenshot?: string) => {
    setEvents(prev => [...prev, { type, timestamp: Date.now(), data, agent: agent || agentRef.current, screenshot }]);
  };

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
        try {
          const trace = JSON.parse(e.data);
          if (trace.type === 'node_start') {
            const node = trace.node;
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
          } else if (trace.type === 'node_end') {
            addEvent('agent_end', `${trace.duration_ms}ms`, trace.node);
          } else if (trace.type === 'state') {
            const state = trace.state || {};
            if (state.response) {
              addEvent('model', state.response.slice(0, 100) + (state.response.length > 100 ? '...' : ''), agentRef.current);
            }
          } else if (trace.type === 'done') {
            const state = trace.state || {};
            if (state.response) {
              addEvent('model', state.response.slice(0, 150) + (state.response.length > 150 ? '...' : ''));
            }
            addEvent('done', `${trace.total_steps} steps`);
          }
        } catch {}
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
    [projectId, binaryPath]
  );

  const cancel = useCallback(() => {
    esRef.current?.close();
    setStreamingText('');
    setCurrentAgent('');
    setIsStreaming(false);
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
  }, []);

  return { send, cancel, isStreaming, streamingText, currentAgent, toolCalls, events, clearEvents, sessionId, newSession, iteration };
}

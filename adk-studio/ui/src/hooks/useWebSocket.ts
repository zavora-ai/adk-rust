import { useCallback, useRef, useState } from 'react';

export type StreamEvent =
  | { type: 'start'; agent: string }
  | { type: 'chunk'; text: string }
  | { type: 'end' }
  | { type: 'error'; message: string };

export function useWebSocket(projectId: string | null) {
  const [isStreaming, setIsStreaming] = useState(false);
  const wsRef = useRef<WebSocket | null>(null);

  const send = useCallback(
    (input: string, apiKey: string, onEvent: (e: StreamEvent) => void) => {
      if (!projectId) return;

      const protocol = window.location.protocol === 'https:' ? 'wss:' : 'ws:';
      const ws = new WebSocket(`${protocol}//${window.location.host}/api/projects/${projectId}/ws`);
      wsRef.current = ws;

      ws.onopen = () => {
        setIsStreaming(true);
        ws.send(JSON.stringify({ input, api_key: apiKey }));
      };

      ws.onmessage = (e) => {
        const event: StreamEvent = JSON.parse(e.data);
        onEvent(event);
        if (event.type === 'end' || event.type === 'error') {
          setIsStreaming(false);
          ws.close();
        }
      };

      ws.onerror = () => {
        onEvent({ type: 'error', message: 'WebSocket error' });
        setIsStreaming(false);
      };

      ws.onclose = () => setIsStreaming(false);
    },
    [projectId]
  );

  const cancel = useCallback(() => {
    wsRef.current?.close();
    setIsStreaming(false);
  }, []);

  return { send, cancel, isStreaming };
}

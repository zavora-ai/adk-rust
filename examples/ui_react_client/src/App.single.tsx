import { useState, useCallback, useEffect } from 'react';
import { Renderer } from './adk-ui-renderer/Renderer';
import type { Component } from './adk-ui-renderer/types';
import { convertA2UIMessage, convertA2UIComponent } from './adk-ui-renderer/a2ui-converter';

const API_BASE = `http://${window.location.hostname}:8080`;
const APP_NAME = 'ui_demo';
const USER_ID = 'user1';

interface Surface {
  surfaceId: string;
  components: Component[];
  dataModel: Record<string, unknown>;
}

function App() {
  const [surface, setSurface] = useState<Surface | null>(null);
  const [sessionId, setSessionId] = useState<string | null>(null);
  const [isLoading, setIsLoading] = useState(false);
  const [error, setError] = useState<string | null>(null);

  const ensureSession = async (): Promise<string> => {
    if (sessionId) return sessionId;

    const response = await fetch(`${API_BASE}/api/apps/${APP_NAME}/users/${USER_ID}/sessions`, {
      method: 'POST',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify({ state: {} }),
    });

    if (!response.ok) throw new Error('Failed to create session');

    const data = await response.json();
    const newSessionId = data.id || data.session_id || `session-${Date.now()}`;
    setSessionId(newSessionId);
    return newSessionId;
  };

  const sendMessage = useCallback(async (message: string) => {
    if (!message.trim() || isLoading) return;

    setIsLoading(true);
    setError(null);

    try {
      const sid = await ensureSession();

      const response = await fetch(`${API_BASE}/api/run/${APP_NAME}/${USER_ID}/${sid}`, {
        method: 'POST',
        headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify({ new_message: message }),
      });

      if (!response.ok) throw new Error(`Server error: ${response.status}`);

      const reader = response.body?.getReader();
      if (!reader) throw new Error('No response body');

      const decoder = new TextDecoder();
      let buffer = '';

      while (true) {
        const { done, value } = await reader.read();
        if (done) break;

        buffer += decoder.decode(value, { stream: true });
        const lines = buffer.split('\n');
        buffer = lines.pop() || '';

        for (const line of lines) {
          if (!line.startsWith('data: ')) continue;
          
          const eventData = line.slice(6);
          if (eventData === '[DONE]') continue;

          try {
            const evt = JSON.parse(eventData);

            // Extract components from function response
            if (evt.content?.parts) {
              for (const part of evt.content.parts) {
                if (part.functionResponse?.name === 'render_screen') {
                  const response = part.functionResponse.response;
                  if (response.components) {
                    // Parse components if they're a JSON string
                    const componentsArray = typeof response.components === 'string' 
                      ? JSON.parse(response.components)
                      : response.components;
                    
                    // Build component map
                    const componentMap = new Map<string, any>();
                    componentsArray.forEach((comp: any) => {
                      const converted = convertA2UIComponent(comp);
                      if (converted) {
                        componentMap.set(converted.id, converted);
                      }
                    });
                    
                    // Resolve children IDs to actual components
                    const resolveChildren = (comp: any): any => {
                      if (comp.children && Array.isArray(comp.children)) {
                        return {
                          ...comp,
                          children: comp.children.map((childId: string) => {
                            const child = componentMap.get(childId);
                            return child ? resolveChildren(child) : null;
                          }).filter(Boolean)
                        };
                      }
                      return comp;
                    };
                    
                    // Find root component and resolve its tree
                    const root = componentMap.get('root');
                    if (root) {
                      const resolvedRoot = resolveChildren(root);
                      setSurface({
                        surfaceId: response.surface_id || 'main',
                        components: [resolvedRoot],
                        dataModel: response.data_model || {},
                      });
                    }
                  }
                }
              }
            }
          } catch (e) {
            console.error('Failed to parse SSE event:', e);
          }
        }
      }
    } catch (err) {
      setError((err as Error).message);
    } finally {
      setIsLoading(false);
    }
  }, [isLoading, sessionId]);

  const handleAction = useCallback(async (actionId: string, data?: Record<string, unknown>) => {
    const message = data 
      ? `Action: ${actionId} with data: ${JSON.stringify(data)}`
      : `Action: ${actionId}`;
    
    await sendMessage(message);
  }, [sendMessage]);

  // Auto-start on mount
  useEffect(() => {
    sendMessage('start');
  }, []);

  if (error) {
    return (
      <div className="min-h-screen flex items-center justify-center bg-gray-50 dark:bg-gray-900">
        <div className="max-w-md w-full bg-white dark:bg-gray-800 rounded-lg shadow-lg p-6">
          <h2 className="text-xl font-bold text-red-600 dark:text-red-400 mb-2">Error</h2>
          <p className="text-gray-700 dark:text-gray-300">{error}</p>
          <button
            onClick={() => {
              setError(null);
              sendMessage('start');
            }}
            className="mt-4 px-4 py-2 bg-blue-600 text-white rounded hover:bg-blue-700"
          >
            Retry
          </button>
        </div>
      </div>
    );
  }

  if (isLoading && !surface) {
    return (
      <div className="min-h-screen flex items-center justify-center bg-gray-50 dark:bg-gray-900">
        <div className="text-center">
          <div className="animate-spin rounded-full h-12 w-12 border-b-2 border-blue-600 mx-auto mb-4"></div>
          <p className="text-gray-600 dark:text-gray-400">Loading...</p>
        </div>
      </div>
    );
  }

  if (!surface) {
    return (
      <div className="min-h-screen flex items-center justify-center bg-gray-50 dark:bg-gray-900">
        <p className="text-gray-600 dark:text-gray-400">No UI to display</p>
      </div>
    );
  }

  return (
    <div className="min-h-screen bg-gray-50 dark:bg-gray-900">
      <div className="max-w-4xl mx-auto p-4">
        <Renderer
          component={surface.components.find(c => c.id === 'root') || surface.components[0]}
          onAction={(event) => {
            if (event.action === 'button_click') {
              handleAction(event.action_id);
            } else if (event.action === 'form_submit') {
              handleAction(event.action_id, event.data);
            }
          }}
        />
      </div>
    </div>
  );
}

export default App;
